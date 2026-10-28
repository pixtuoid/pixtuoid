use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU16;
use std::sync::Arc;

use enum_map::EnumMap;
use serde::Deserialize;
use strum::VariantArray;
use vec1::Vec1;

use crate::grid::Grid;
use crate::sprite::error::{ColorError, KeySite, LineError, PackError, SpriteError};
use crate::sprite::{
    Frame, HEAD_MARK, HeadMark, HeadView, IndexedFrame, Mark, PALETTE_CAPACITY, Palette,
    PaletteIndex, Pixel, Rgb, Sprite,
};

type Result<T, E = PackError> = std::result::Result<T, E>;

struct Block {
    header: usize,
    rows: Vec<Vec<PaletteIndex>>,
    marks: Vec<(Mark, usize)>,
}

/// Parse a `.sprite` text file: one indexed frame per `@frame N` block, each
/// with the `@mark <name> <x> <y>` lines its block carries.
fn parse_indexed(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_value_rejects_sign_prefixed_or_non_hex() {
        assert!(parse_palette_value("#+f0102").is_err());
        assert!(parse_palette_value("#-f0102").is_err());
        assert!(parse_palette_value("#abXY12").is_err());
        assert!(parse_palette_value("#Ff0102").unwrap().is_some());
        assert!(parse_palette_value("transparent").unwrap().is_none());
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

    /// A pack of `extra` tables over a palette of the seven material keys and
    /// a stray `x`; `files` holds the buildings' sprites.
    fn city_pack(extra: &str, files: &[(&str, &str)]) -> Result<Pack> {
        load_filled_pack(
            &format!(
                "[palette]\n\".\"=\"transparent\"\n\
                 \"F\"=\"#202020\"\n\"f\"=\"#181818\"\n\"R\"=\"#303030\"\n\"W\"=\"#404040\"\n\
                 \"M\"=\"#101010\"\n\"D\"=\"#282828\"\n\"L\"=\"#d08050\"\n\"x\"=\"#ffffff\"\n\
                 {extra}"
            ),
            files,
        )
    }

    const CITY: &str = "[city]\nfacade=\"F\"\nshade=\"f\"\nroof=\"R\"\nglass=\"W\"\n\
                        mullion=\"M\"\ndetail=\"D\"\nsign=\"L\"\n";
    /// A 2x3 tower: one window of two glass pixels over a wall.
    const TOWER: &str = "@frame 0\nR R\nW W\nF f\n";
    /// The tower at 2x, drawn with a mullion splitting its window in two.
    const TOWER_2X: &str = "@frame 0\nR R R R\nR R R R\nW M W W\nW M W W\nF F f f\nF F f f\n";

    #[test]
    fn a_building_is_its_base_and_its_density_variants() {
        let pack = city_pack(
            &format!(
                "{CITY}[buildings.tower]\nsprite=\"t.sprite\"\nplanes=[\"mid\", \"near\"]\n\
                 [buildings.\"tower@2x\"]\nsprite=\"t2.sprite\"\n"
            ),
            &[("t.sprite", TOWER), ("t2.sprite", TOWER_2X)],
        )
        .expect("the city pack loads");
        let tower = pack.buildings().next().expect("the tower");
        assert_eq!((tower.name(), tower.size()), ("tower", (2, 3)));
        assert!(tower.stands_in(CityPlane::Mid) && tower.stands_in(CityPlane::Near));
        let d = |n| Density::new(n).expect("nonzero");
        let base = tower.base();
        assert!(tower.variant(d(1)).is_none(), "the base is no variant");
        assert_eq!(
            base.windows(),
            [vec![(0, 1), (1, 1)]],
            "one run of glass, one window"
        );
        let dense = tower.variant(d(2)).expect("the 2x variant");
        assert_eq!(
            dense.windows().len(),
            2,
            "each density finds its own windows: the mullion splits this one"
        );
        assert!(tower.variant(d(4)).is_none());
        assert_eq!(pack.city_materials().key(Material::Glass), 'W');
    }

    #[test]
    fn every_material_has_its_own_place() {
        for (i, m) in Material::ALL.into_iter().enumerate() {
            assert_eq!(m.index(), i, "{m:?}");
        }
    }

    #[test]
    fn a_building_outside_its_rules_is_rejected() {
        let base = "[buildings.tower]\nsprite=\"t.sprite\"\nplanes=[\"near\"]\n";
        let load = |extra: &str, art: &str, dense: &str| {
            city_pack(extra, &[("t.sprite", art), ("t2.sprite", dense)])
        };
        let ok = format!("{CITY}{base}[buildings.\"tower@2x\"]\nsprite=\"t2.sprite\"\n");
        assert!(load(&ok, TOWER, TOWER_2X).is_ok());
        let rejected = [
            (
                format!("{CITY}{base}"),
                "@frame 0\nF x\n",
                "a key no material names",
            ),
            (
                format!("{CITY}[buildings.tower]\nsprite=\"t.sprite\"\nplanes=[\"far\"]\n"),
                TOWER,
                "the far plane is the painter's own",
            ),
            (
                format!("{CITY}[buildings.tower]\nsprite=\"t.sprite\"\n"),
                TOWER,
                "a base naming no planes",
            ),
            (
                format!("{CITY}[buildings.tower]\nsprite=\"t.sprite\"\nplanes=[]\n"),
                TOWER,
                "a base standing in no plane",
            ),
            (
                format!("{CITY}[buildings.\"tower@1x\"]\nsprite=\"t.sprite\"\nplanes=[\"near\"]\n"),
                TOWER,
                "a base named like a variant",
            ),
            (
                format!(
                    "{CITY}[buildings.\"tower@big\"]\nsprite=\"t.sprite\"\nplanes=[\"near\"]\n"
                ),
                TOWER,
                "a base named with a variant's mark",
            ),
            (
                format!("{CITY}{base}"),
                "@frame 0\nR R\nW W\nF f\n@frame 1\nR R\nW W\nF f\n",
                "a building of two frames",
            ),
            (
                format!("{}{base}", CITY.replace("sign=\"L\"", "sign=\".\"")),
                TOWER,
                "a material drawn in a transparent key",
            ),
            (
                format!("{CITY}[buildings.\"tower@2x\"]\nsprite=\"t2.sprite\"\n"),
                TOWER,
                "a variant with no base",
            ),
            (
                format!(
                    "{CITY}{base}[buildings.\"tower@2x\"]\nsprite=\"t2.sprite\"\nplanes=[\"mid\"]\n"
                ),
                TOWER,
                "a variant naming its own planes",
            ),
            (
                format!("{CITY}{base}[buildings.\"tower@2x\"]\nsprite=\"t.sprite\"\n"),
                TOWER,
                "a variant not twice its base",
            ),
            (
                format!("{}{base}", CITY.replace("sign=\"L\"", "sign=\"F\"")),
                TOWER,
                "two materials in one key",
            ),
            (
                format!("{}{base}", CITY.replace("sign=\"L\"\n", "")),
                TOWER,
                "a material with no key",
            ),
            (
                format!("{CITY}neon=\"L\"\n{base}"),
                TOWER,
                "a key naming no material",
            ),
        ];
        for (extra, art, why) in rejected {
            assert!(load(&extra, art, TOWER_2X).is_err(), "{why}");
        }
    }

    /// A pack whose `seated` draws `f.sprite`, plus `extra` tables; `frames`
    /// holds every file by name. As a caller sees the failure: through
    /// `anyhow`, whose `{:#}` walks the chain.
    fn hair_pack(extra: &str, frames: &[(&str, &str)]) -> anyhow::Result<Pack> {
        let toml = format!(
            "[palette]\n\".\"=\"transparent\"\n\
             \"H\"=\"#28140a\"\n\"k\"=\"#101010\"\n\
             [animations.seated]\nframes=[\"f.sprite\"]\nframe_ms=100\n{extra}"
        );
        Ok(load_filled_pack(&toml, frames)?)
    }

    #[test]
    fn a_mark_names_a_point_of_its_own_frame() {
        let pack = hair_pack(
            "",
            &[(
                "f.sprite",
                "@frame 0\n@mark head.front 1 0\n@mark cup 0 1\nH H\nH H\n@frame 1\nH H\nH H\n",
            )],
        )
        .expect("loads");
        let seated = pack.piece(Piece::Seated);
        let head = seated.head(0).expect("frame 0's head");
        assert_eq!((head.view, head.x, head.y), (HeadView::Front, 1, 0));
        let names: Vec<_> = seated.marks(0).iter().map(Mark::name).collect();
        assert_eq!(names, ["head.front", "cup"]);
        assert_eq!(seated.head(1), None, "an unmarked frame has no head");
        assert!(seated.marks(1).is_empty());
    }

    #[test]
    fn a_mark_outside_its_frame_or_out_of_its_rules_is_rejected() {
        for bad in [
            "@mark cup 2 0",
            "@mark head.up 0 0",
            "@mark cup 0",
            "@mark Cup 0 0",
            "@mark cup 0 0\n@mark cup 1 0",
            "@mark head.front 0 0\n@mark head.back 1 0",
        ] {
            let err =
                hair_pack("", &[("f.sprite", &format!("@frame 0\n{bad}\nH H\n"))]).expect_err(bad);
            assert!(format!("{err:#}").contains("mark"), "{bad}: {err:#}");
        }
    }

    #[test]
    fn a_hairstyle_loads_its_layers_per_view_at_its_density() {
        let pack = hair_pack(
            "[characters]\noutline=\"k\"\n\
             [hairstyles.\"mop@2x\"]\nfront={ behind=\"b.sprite\", over=\"o.sprite\" }\nback={ over=\"o2.sprite\" }\n",
            &[
                ("f.sprite", "@frame 0\nH\n"),
                ("b.sprite", "@frame 0\n@mark head.front 0 1\nH\nH\n"),
                ("o.sprite", "@frame 0\n@mark head.front 0 0\nH\n"),
                ("o2.sprite", "@frame 0\n@mark head.back 0 0\nH\n"),
            ],
        )
        .expect("loads");
        let styles: Vec<_> = pack.hairstyles().collect();
        assert_eq!(styles.len(), 1);
        let mop = styles[0];
        assert_eq!((mop.name(), mop.density().get()), ("mop", 2));
        assert!(pack.hairstyle("mop", mop.density()).is_some());
        let front = mop.layers(HeadView::Front).expect("a front view");
        assert_eq!(front.behind().and_then(|l| l.head(0)).map(|h| h.y), Some(1));
        assert!(front.over().is_some());
        assert!(
            mop.layers(HeadView::Back)
                .is_some_and(|l| l.behind().is_none())
        );
        assert!(mop.layers(HeadView::Side).is_none());
        assert_eq!(
            pack.character_outline(),
            Rgb {
                r: 16,
                g: 16,
                b: 16
            }
        );
    }

    #[test]
    fn a_hairstyle_out_of_its_rules_is_rejected() {
        let f = ("f.sprite", "@frame 0\nH\n");
        let front = ("o.sprite", "@frame 0\n@mark head.front 0 0\nH\n");
        type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
        let cases: [Case; 5] = [
            (
                "[hairstyles.\"mop@2x\"]\nfront={ over=\"o.sprite\" }\n",
                &[f, ("o.sprite", "@frame 0\n@mark head.back 0 0\nH\n")],
                "a back head on a front layer",
            ),
            (
                "[hairstyles.mop]\nfront={ over=\"o.sprite\" }\n",
                &[f, front],
                "a style without a density",
            ),
            (
                "[hairstyles.\"mop@2x\"]\nfront={ over=\"o.sprite\" }\n\
                 [hairstyles.\"bun@4x\"]\nfront={ over=\"o.sprite\" }\n",
                &[f, front],
                "styles differing between densities",
            ),
            (
                "[characters]\noutline=\".\"\n",
                &[f],
                "a transparent outline",
            ),
            (
                "[characters]\noutline=\"q\"\n",
                &[f],
                "an outline no palette key names",
            ),
        ];
        for (extra, files, why) in cases {
            assert!(hair_pack(extra, files).is_err(), "{why}");
        }
        assert!(
            hair_pack(
                "[hairstyles.\"mop@2x\"]\nfront={ over=\"o.sprite\" }\n\
                 [hairstyles.\"mop@4x\"]\nfront={ over=\"o.sprite\" }\n",
                &[f, front],
            )
            .is_ok(),
            "one style at two densities"
        );
    }

    fn ramp_pack(palette: &str, ramps: &str, sprite: &str) -> anyhow::Result<Pack> {
        let toml = format!(
            "[palette]\n{palette}\n\
             [ramps]\n{ramps}\n\
             [animations.seated]\nframes=[\"f.sprite\"]\nframe_ms=100\n"
        );
        Ok(load_filled_pack(&toml, &[("f.sprite", sprite)])?)
    }

    const HAIR: Rgb = Rgb {
        r: 40,
        g: 20,
        b: 10,
    };

    #[test]
    fn ramp_keys_draw_as_a_step_of_their_base() {
        let pack = ramp_pack(
            "\"H\"=\"#28140a\"",
            "\"h\" = { of = \"H\", level = -1 }",
            "@frame 0\nH h",
        )
        .expect("pack builds");
        let frame = pack.piece(Piece::Seated).first();
        assert_eq!(frame.get(0, 0).copied().flatten(), Some(HAIR));
        assert_eq!(frame.get(1, 0).copied().flatten(), Some(HAIR.ramp(-1)));
    }

    /// `X` shares `B`'s color and stays; `h` is never named and follows `H`;
    /// `.` is transparent and stays so.
    #[test]
    fn a_recolor_replaces_keys_not_colors() {
        let pack = ramp_pack(
            "\"B\"=\"#2e62cf\"\n\"X\"=\"#2e62cf\"\n\"H\"=\"#28140a\"\n\".\"=\"transparent\"",
            "\"h\" = { of = \"H\", level = -1 }",
            "@frame 0\nB X h .",
        )
        .expect("pack builds");
        let seated = pack.piece(Piece::Seated);
        let (red, blond) = (
            Rgb { r: 200, g: 0, b: 0 },
            Rgb {
                r: 200,
                g: 160,
                b: 80,
            },
        );
        let overrides = [
            ('B', Some(red)),
            ('H', Some(blond)),
            ('.', Some(red)),
            ('Q', None),
        ];
        let out = seated.recolorable_at(0).recolored(&overrides);
        let shirt = pack.palette().get('B').flatten();
        assert_eq!(
            out.as_slice(),
            &[Some(red), shirt, Some(blond.ramp(-1)), None][..]
        );
        assert_eq!(
            seated.recolorable_at(1).recolored(&overrides).as_slice(),
            out.as_slice(),
            "an index past the last frame wraps to the first"
        );
        assert_eq!(
            seated.frames()[0].as_slice(),
            &[shirt, shirt, Some(HAIR.ramp(-1)), None][..],
            "the pack's own colors are untouched"
        );
    }

    #[test]
    fn a_ramp_may_step_as_far_as_the_bound_either_way() {
        for level in [MAX_RAMP_LEVEL, -MAX_RAMP_LEVEL] {
            let ramps = format!("\"h\" = {{ of = \"H\", level = {level} }}");
            ramp_pack("\"H\"=\"#28140a\"", &ramps, "@frame 0\nh").expect(&ramps);
        }
    }

    #[test]
    fn a_ramp_rejects_a_malformed_declaration() {
        for (ramps, needle) in [
            ("\"h\" = { of = \"Q\", level = -1 }", "no opaque color"),
            ("\"h\" = { of = \"P\", level = -1 }", "no opaque color"),
            (
                "\"h\" = { of = \"H\", level = -1 }\n\"k\" = { of = \"h\", level = -1 }",
                "no opaque color",
            ),
            ("\"H2\" = { of = \"H\", level = -1 }", "one character"),
            ("\"X\" = { of = \"H\", level = -1 }", "both"),
            ("\"h\" = { of = \"H\", level = 0 }", "nonzero"),
            ("\"h\" = { of = \"H\", level = 11 }", "within"),
            ("\"h\" = { of = \"H\", level = -11 }", "within"),
            (
                "\"h\" = { of = \"H\", level = -1, typo = 1 }",
                "unknown field",
            ),
        ] {
            let err = ramp_pack(
                "\"H\"=\"#28140a\"\n\"X\"=\"#010203\"\n\"P\"=\"transparent\"",
                ramps,
                "@frame 0\nH",
            )
            .expect_err(ramps);
            assert!(format!("{err:#}").contains(needle), "{ramps}: {err:#}");
        }
    }

    /// A key that is an ESC or a bidi override comes out escaped in every ramp
    /// error, never raw.
    #[test]
    fn a_control_character_key_is_escaped_in_every_ramp_error() {
        for ramps in [
            "\"\\u001B\" = { of = \"Q\", level = -1 }",
            "\"h\" = { of = \"\\u202E\", level = -1 }",
            "\"\\u001B\\u001B\" = { of = \"H\", level = -1 }",
            "\"\\u001B\" = { of = \"H\", level = 11 }",
        ] {
            let err = ramp_pack("\"H\"=\"#28140a\"", ramps, "@frame 0\nH").expect_err(ramps);
            let msg = format!("{err:#}");
            assert!(
                !msg.contains('\u{1b}') && !msg.contains('\u{202e}'),
                "{ramps}: raw control character in {msg:?}"
            );
        }
    }

    #[test]
    fn a_palette_past_what_a_frame_can_index_is_rejected() {
        // the filler's own keys count toward the capacity
        let keys: String = FILLER_KEYS
            .into_iter()
            .chain('\u{100}'..)
            .take(PALETTE_CAPACITY + 1)
            .map(|k| format!("\"{k}\"=\"#010203\"\n"))
            .collect();
        let err = ramp_pack(&keys, "", "@frame 0\n\u{100}").expect_err("over capacity");
        assert!(format!("{err:#}").contains("at most"), "{err:#}");
        let fits: String = keys
            .lines()
            .take(PALETTE_CAPACITY)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(ramp_pack(&fits, "", "@frame 0\n\u{100}").is_ok());
        let err = ramp_pack(
            &fits,
            "\"h\" = { of = \"\u{100}\", level = -1 }",
            "@frame 0\n\u{100}",
        )
        .expect_err("a ramp is a key too");
        assert!(format!("{err:#}").contains("at most"), "{err:#}");
    }
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackToml {
    /// Ordered, like `ramps`, so a pack loads the same way every time: the same
    /// indices, and the same key reported first when several are bad.
    palette: BTreeMap<String, String>,
    #[serde(default)]
    ramps: BTreeMap<String, RampToml>,
    animations: HashMap<String, AnimationToml>,
    city: BTreeMap<String, String>,
    #[serde(default)]
    buildings: BTreeMap<String, BuildingToml>,
    characters: CharactersToml,
    #[serde(default)]
    hairstyles: BTreeMap<String, HairstyleToml>,
    /// Each icon's one-frame art, by name.
    #[serde(default)]
    icons: BTreeMap<String, String>,
}

/// One `[buildings.<name>]` table (the base art, with the planes the building
/// stands in) or `[buildings."<name>@<N>x"]` (a density variant of it).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildingToml {
    sprite: String,
    planes: Option<Vec<String>>,
}

/// The `[characters]` table: what every marked character frame is finished with.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CharactersToml {
    /// The palette key of the one line round a marked frame, dressed or bare.
    outline: String,
}

/// One `[hairstyles."<name>@<N>x"]` table: a pair of layers per view.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HairstyleToml {
    front: Option<HairLayersToml>,
    back: Option<HairLayersToml>,
    side: Option<HairLayersToml>,
    crown: Option<HairLayersToml>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HairLayersToml {
    behind: Option<String>,
    over: Option<String>,
}

/// One `[ramps]` entry, loaded by [`Palette::insert_ramp`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RampToml {
    of: String,
    level: i8,
}

#[derive(Debug, Deserialize)]
struct AnimationToml {
    frames: Vec<String>,
    frame_ms: u32,
    #[serde(default)]
    stride: Option<std::num::NonZeroU16>,
}

/// A loaded sprite pack: a palette, every [`Piece`] and the density variants
/// that redraw them, the hairstyles that dress the characters, and the city
/// behind the windows.
#[derive(Debug, Clone)]
pub struct Pack {
    palette: Arc<Palette>,
    /// A pack that lacks a piece does not load.
    pieces: EnumMap<Piece, Sprite>,
    /// Each piece's density variants, each of which redraws it
    /// ([`variant_redraws`]): one that does not, does not load.
    variants: EnumMap<Piece, BTreeMap<Density, Sprite>>,
    /// A walk without one does not load.
    strides: EnumMap<Walk, NonZeroU16>,
    buildings: BTreeMap<String, Building>,
    city_materials: CityMaterials,
    hairstyles: BTreeMap<String, Hairstyle>,
    character_outline: Rgb,
    icons: BTreeMap<String, Sprite>,
    /// [`Pack::density_variants`], counted once at load: a painter asks for it
    /// every frame.
    densities: Vec<Density>,
}

/// A material a `[buildings]` sprite is drawn in. A painter gives each its
/// colour from the sky, the theme and the building's depth, so one drawing
/// serves day and night alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Material {
    /// The lit face of a wall.
    Facade,
    /// The face turned from the light.
    Shade,
    /// A roof line or cornice.
    Roof,
    /// A window's glass: each connected run of it is one window, lit or not.
    Glass,
    /// The frame between panes.
    Mullion,
    /// Rooftop plant: a tank, a mast, a unit.
    Detail,
    /// A sign's panel, lit after dark.
    Sign,
}

impl Material {
    /// Its name as `[city]` keys it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Material::Facade => "facade",
            Material::Shade => "shade",
            Material::Roof => "roof",
            Material::Glass => "glass",
            Material::Mullion => "mullion",
            Material::Detail => "detail",
            Material::Sign => "sign",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Material::ALL.into_iter().find(|m| m.name() == name)
    }

    /// Its place in [`Material::ALL`]; a new material fails to compile here
    /// until it has one.
    fn index(self) -> usize {
        match self {
            Material::Facade => 0,
            Material::Shade => 1,
            Material::Roof => 2,
            Material::Glass => 3,
            Material::Mullion => 4,
            Material::Detail => 5,
            Material::Sign => 6,
        }
    }

    /// Every material, in declaration order.
    pub const ALL: [Material; 7] = [
        Material::Facade,
        Material::Shade,
        Material::Roof,
        Material::Glass,
        Material::Mullion,
        Material::Detail,
        Material::Sign,
    ];
}

/// The palette key each [`Material`] is drawn in, from `[city]`.
#[derive(Debug, Clone)]
pub struct CityMaterials([char; Material::ALL.len()]);

impl CityMaterials {
    /// The key `material` is drawn in.
    pub fn key(&self, material: Material) -> char {
        self.0[material.index()]
    }
}

/// A depth plane of the city a building may stand in: the far plane is the
/// painter's own silhouettes, so a pack draws only these two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CityPlane {
    /// The middle distance.
    Mid,
    /// The nearest buildings, their feet below the sill.
    Near,
}

impl CityPlane {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "mid" => Some(CityPlane::Mid),
            "near" => Some(CityPlane::Near),
            _ => None,
        }
    }
}

/// One building of the city behind the office's windows, loaded from
/// `[buildings.<name>]` and its `[buildings."<name>@<N>x"]` variants: drawn
/// only in the `[city]` materials, and the planes it stands in.
#[derive(Debug, Clone)]
pub struct Building {
    name: String,
    planes: Vec<CityPlane>,
    base: BuildingArt,
    variants: BTreeMap<Density, BuildingArt>,
}

impl Building {
    /// The building's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether it may stand in `plane`.
    pub fn stands_in(&self, plane: CityPlane) -> bool {
        self.planes.contains(&plane)
    }

    /// Its size in logical units: its base art's.
    pub fn size(&self) -> (u16, u16) {
        self.base
            .sprite
            .frames()
            .first()
            .map_or((0, 0), |f| (f.width(), f.height()))
    }

    /// Its base art, at 1x.
    pub fn base(&self) -> &BuildingArt {
        &self.base
    }

    /// Its variant drawn at `density`, above its [`base`](Self::base).
    pub fn variant(&self, density: Density) -> Option<&BuildingArt> {
        self.variants.get(&density)
    }
}

/// A building drawn at one density: its one-frame sprite, and its windows.
#[derive(Debug, Clone)]
pub struct BuildingArt {
    sprite: Sprite,
    windows: Vec<Vec<(u16, u16)>>,
}

impl BuildingArt {
    /// The one-frame drawing, in the `[city]` materials.
    pub fn sprite(&self) -> &Sprite {
        &self.sprite
    }

    /// Its windows: each a connected run of [`Material::Glass`] pixels, which
    /// lights as one.
    pub fn windows(&self) -> &[Vec<(u16, u16)>] {
        &self.windows
    }
}

/// A hairstyle: per view, the layers that dress a character frame whose head
/// faces that way, laid mark on mark. Loaded from `[hairstyles."<name>@<N>x"]`,
/// so it only ever dresses art at its own density: the classic `1x` art is
/// never dressed.
#[derive(Debug, Clone)]
pub struct Hairstyle {
    name: String,
    density: Density,
    views: [Option<HairLayers>; HeadView::ALL.len()],
}

impl Hairstyle {
    /// The style's name, without its density.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The density of the art it dresses.
    pub fn density(&self) -> Density {
        self.density
    }

    /// Its layers for a head facing `view`.
    pub fn layers(&self, view: HeadView) -> Option<&HairLayers> {
        self.views[view.index()].as_ref()
    }
}

/// One view's layers: `behind` goes under the body, `over` on top. Each is a
/// one-frame sprite whose head mark lands on the body's.
#[derive(Debug, Clone)]
pub struct HairLayers {
    behind: Option<Sprite>,
    over: Option<Sprite>,
}

impl HairLayers {
    /// The layer under the body.
    pub fn behind(&self) -> Option<&Sprite> {
        self.behind.as_ref()
    }

    /// The layer over the body.
    pub fn over(&self) -> Option<&Sprite> {
        self.over.as_ref()
    }
}

impl Pack {
    /// The buildings of the city behind the windows, in name order.
    pub fn buildings(&self) -> impl Iterator<Item = &Building> {
        self.buildings.values()
    }

    /// The keys the buildings are drawn in, from `[city]`.
    pub fn city_materials(&self) -> &CityMaterials {
        &self.city_materials
    }

    /// The pack's hairstyles, every density of each, in name order.
    pub fn hairstyles(&self) -> impl Iterator<Item = &Hairstyle> {
        self.hairstyles.values()
    }

    /// The style `name` at `density`, where the pack draws one.
    pub fn hairstyle(&self, name: &str, density: Density) -> Option<&Hairstyle> {
        self.hairstyles.get(&format!("{name}@{density}x"))
    }

    /// The colour of the one line round every marked character frame at a
    /// density of 2 and up, dressed or bare, from `[characters]`.
    pub fn character_outline(&self) -> Rgb {
        self.character_outline
    }

    /// The icon `name`'s art, from `[icons]`.
    pub fn icon(&self, name: &str) -> Option<&Sprite> {
        self.icons.get(name)
    }

    /// The names of the pack's icons, in order: the icon tests' check that
    /// the pack draws no icon nothing names.
    #[doc(hidden)]
    pub fn icon_names(&self) -> impl Iterator<Item = &str> {
        self.icons.keys().map(String::as_str)
    }

    /// The palette the pack's frames were drawn with.
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// The art of `piece`.
    pub fn piece(&self, piece: Piece) -> &Sprite {
        &self.pieces[piece]
    }

    /// `piece` redrawn at each density the pack draws it at, above its
    /// [`piece`](Self::piece) art.
    pub fn variants_of(&self, piece: Piece) -> &BTreeMap<Density, Sprite> {
        &self.variants[piece]
    }

    /// Every density variant, in piece then density order.
    pub fn variants(&self) -> impl Iterator<Item = (Piece, Density, &Sprite)> {
        self.variants
            .iter()
            .flat_map(|(p, by)| by.iter().map(move |(&d, s)| (p, d, s)))
    }

    /// The base-grid pixels `walk` covers in one full cycle of its frames:
    /// every walk declares one, or the pack does not load.
    pub fn stride(&self, walk: Walk) -> NonZeroU16 {
        self.strides[walk]
    }

    /// The densest of [`Pack::density_variants`], or [`Density::ONE`] when the
    /// pack ships none.
    pub fn max_density_variant(&self) -> Density {
        self.densities.first().copied().unwrap_or(Density::ONE)
    }

    /// The densities this pack's variants are drawn at, densest first, each
    /// once.
    ///
    /// A painter picks its render scale against these (the scene's
    /// `RenderScale::fit`), since a variant only lands at a scale its density
    /// divides. Only a variant of a registered animation that redraws its base
    /// (`variant_redraws`) counts: a stray key names nothing a painter asks
    /// for, and every renderer skips a variant that does not redraw its base.
    pub fn density_variants(&self) -> &[Density] {
        &self.densities
    }

    fn count_densities(&mut self) {
        let densities: BTreeSet<Density> = self.variants().map(|(_, d, _)| d).collect();
        self.densities = densities.into_iter().rev().collect();
    }
}

fn build_pack(parsed: PackToml, get_src: &mut dyn FnMut(&str) -> Result<String>) -> Result<Pack> {
    let palette = Arc::new(build_palette(&parsed.palette, &parsed.ramps)?);
    let Pieces {
        pieces,
        variants,
        strides,
    } = build_pieces(parsed.animations, &palette, get_src)?;

    let city_materials = {
        let c = parsed.city;
        if let Some(other) = c.keys().find(|k| Material::from_name(k).is_none()) {
            return Err(PackError::CityUnknownMaterial {
                name: other.clone(),
            });
        }
        let mut keys = [' '; Material::ALL.len()];
        for material in Material::ALL {
            let Some(key) = c.get(material.name()) else {
                return Err(PackError::CityMissingMaterial { material });
            };
            let what = KeySite::CityMaterial;
            let key = single_char(key, what)?;
            if !matches!(palette.get(key), Some(Some(_))) {
                return Err(PackError::NotOpaque { what, key });
            }
            if keys[..material.index()].contains(&key) {
                return Err(PackError::CityKeyShared { key });
            }
            keys[material.index()] = key;
        }
        CityMaterials(keys)
    };
    let mut buildings: BTreeMap<String, Building> = BTreeMap::new();
    let (building_variants, bases): (Vec<_>, Vec<_>) = parsed
        .buildings
        .into_iter()
        .partition(|(key, _)| split_density_variant(key).is_some());
    for (key, building) in bases.into_iter().chain(building_variants) {
        let materials = &city_materials;
        let (name, density) = split_density_variant(&key).unwrap_or((&key, Density::ONE));
        if density == Density::ONE && key.contains(DENSITY_VARIANT_SEP) {
            return Err(PackError::BuildingDensity { key });
        }
        let art = building_art(&building.sprite, &palette, materials, get_src)?;
        match (density, building.planes) {
            (Density::ONE, Some(planes)) if !planes.is_empty() => {
                let planes = planes
                    .iter()
                    .map(|p| {
                        CityPlane::from_name(p).ok_or_else(|| PackError::BuildingPlane {
                            key: key.clone(),
                            plane: p.clone(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                buildings.insert(
                    key.clone(),
                    Building {
                        name: key.clone(),
                        planes,
                        base: art,
                        variants: BTreeMap::new(),
                    },
                );
            }
            (Density::ONE, _) => return Err(PackError::BuildingWithoutPlanes { key }),
            (_, Some(_)) => return Err(PackError::VariantPlanes { key }),
            (_, None) => {
                let no_base = |key| PackError::VariantWithoutBase {
                    base: name.to_owned(),
                    key,
                };
                let Some(base) = buildings.get_mut(name) else {
                    return Err(no_base(key.clone()));
                };
                if !variant_redraws(&base.base.sprite, density, &art.sprite) {
                    let (base_w, base_h) = base.size();
                    return Err(PackError::VariantSize {
                        key,
                        density,
                        base_w,
                        base_h,
                    });
                }
                base.variants.insert(density, art);
            }
        }
    }

    let character_outline = {
        let what = KeySite::CharacterOutline;
        let key = single_char(&parsed.characters.outline, what)?;
        match palette.get(key) {
            Some(Some(rgb)) => rgb,
            _ => return Err(PackError::NotOpaque { what, key }),
        }
    };
    let mut hairstyles = BTreeMap::new();
    for (key, style) in parsed.hairstyles {
        let Some((name, density)) = split_density_variant(&key) else {
            return Err(PackError::HairstyleDensity { key });
        };
        let mut layer = |view: HeadView, fname: &str| -> Result<Sprite> {
            let src = get_src(fname)?;
            let Some(frame) = one_frame(decode(fname, &src, &palette)?) else {
                return Err(PackError::HairLayerFrames {
                    file: fname.to_owned(),
                });
            };
            if frame.1.iter().find_map(HeadMark::of).map(|h| h.view) != Some(view) {
                return Err(PackError::HairLayerHead {
                    file: fname.to_owned(),
                    view,
                });
            }
            Ok(Sprite::new(Vec1::new(frame), Arc::clone(&palette), 0, None))
        };
        let mut views: [Option<HairLayers>; HeadView::ALL.len()] = Default::default();
        for (view, layers) in [
            (HeadView::Front, style.front),
            (HeadView::Back, style.back),
            (HeadView::Side, style.side),
            (HeadView::Crown, style.crown),
        ] {
            let Some(l) = layers else { continue };
            views[view.index()] = Some(HairLayers {
                behind: l.behind.as_deref().map(|f| layer(view, f)).transpose()?,
                over: l.over.as_deref().map(|f| layer(view, f)).transpose()?,
            });
        }
        hairstyles.insert(
            key.clone(),
            Hairstyle {
                name: name.to_owned(),
                density,
                views,
            },
        );
    }
    // A style changing with the density the renderer lands on would change an
    // agent's look with the window's size.
    let names_at = |d| -> BTreeSet<&str> {
        hairstyles
            .values()
            .filter(|s| s.density == d)
            .map(|s| s.name.as_str())
            .collect()
    };
    let densities: BTreeSet<_> = hairstyles.values().map(|s| s.density).collect();
    if let [first, rest @ ..] = densities.iter().copied().collect::<Vec<_>>().as_slice()
        && let Some(d) = rest.iter().find(|&&d| names_at(d) != names_at(*first))
    {
        return Err(PackError::HairstylesDiffer {
            first: *first,
            other: *d,
        });
    }

    let mut icons = BTreeMap::new();
    for (name, fname) in parsed.icons {
        let src = get_src(&fname)?;
        let Some(frame) = one_frame(decode(&fname, &src, &palette)?) else {
            return Err(PackError::IconFrames { file: fname });
        };
        let art = Sprite::new(Vec1::new(frame), Arc::clone(&palette), 0, None);
        icons.insert(name, art);
    }

    let mut pack = Pack {
        palette,
        pieces,
        variants,
        strides,
        buildings,
        city_materials,
        hairstyles,
        character_outline,
        icons,
        densities: Vec::new(),
    };
    pack.count_densities();
    Ok(pack)
}

/// The manifest's `[animations]`: every [`Piece`], each [`Walk`]'s stride,
/// and the density variants, each of which redraws its piece.
struct Pieces {
    pieces: EnumMap<Piece, Sprite>,
    variants: EnumMap<Piece, BTreeMap<Density, Sprite>>,
    strides: EnumMap<Walk, NonZeroU16>,
}

fn build_pieces(
    animations: HashMap<String, AnimationToml>,
    palette: &Arc<Palette>,
    get_src: &mut dyn FnMut(&str) -> Result<String>,
) -> Result<Pieces> {
    let mut bases: BTreeMap<Piece, Sprite> = BTreeMap::new();
    let mut variants = BTreeMap::new();
    // by key, so a pack reports the same bad key first every load
    let animations: BTreeMap<String, AnimationToml> = animations.into_iter().collect();
    for (key, anim) in animations {
        let Some((piece, density)) = parse_key(&key) else {
            return Err(PackError::UnknownAnimation { key });
        };
        let mut frames = Vec::new();
        for fname in &anim.frames {
            let src = get_src(fname)?;
            let mut decoded = decode(fname, &src, palette)?;
            frames.append(&mut decoded);
        }
        let need = density.map_or(piece.min_frames(), |_| 1);
        let have = frames.len();
        let frames = match Vec1::try_from_vec(frames) {
            Ok(frames) if have >= need => frames,
            _ => return Err(PackError::TooFewFrames { key, need, have }),
        };
        let sprite = Sprite::new(frames, Arc::clone(palette), anim.frame_ms, anim.stride);
        match density {
            None => {
                bases.insert(piece, sprite);
            }
            Some(density) => {
                variants.insert((piece, density), (key, sprite));
            }
        }
    }
    let pieces = EnumMap::try_from_fn(|piece| {
        bases
            .remove(&piece)
            .ok_or(PackError::MissingPiece { piece })
    })?;
    let strides = EnumMap::try_from_fn(|walk: Walk| {
        pieces[walk.piece()]
            .stride()
            .ok_or(PackError::WalkWithoutStride { walk: walk.piece() })
    })?;
    let mut redrawn: EnumMap<Piece, BTreeMap<Density, Sprite>> = EnumMap::default();
    for ((piece, density), (key, variant)) in variants {
        let base = &pieces[piece];
        if !variant_redraws(base, density, &variant) {
            return Err(PackError::VariantDoesNotRedraw {
                key,
                base_frames: base.frames().len(),
                variant_frames: variant.frames().len(),
            });
        }
        redrawn[piece].insert(density, variant);
    }
    Ok(Pieces {
        pieces,
        variants: redrawn,
        strides,
    })
}

/// A building's one-frame sprite `fname`, checked to draw only in the
/// `[city]` materials, with its windows found.
fn building_art(
    fname: &str,
    palette: &Arc<Palette>,
    materials: &CityMaterials,
    get_src: &mut dyn FnMut(&str) -> Result<String>,
) -> Result<BuildingArt> {
    let src = get_src(fname)?;
    let Some(one) = one_frame(decode(fname, &src, palette)?) else {
        return Err(PackError::BuildingFrames {
            file: fname.to_owned(),
        });
    };
    let frame = &one.0;
    let pixels = palette.resolved();
    let index = |m: Material| palette.index_of(materials.key(m));
    let drawn: Vec<_> = Material::ALL.into_iter().filter_map(index).collect();
    let grid = &frame.0;
    for (i, &p) in grid.as_slice().iter().enumerate() {
        let p = usize::from(p);
        if pixels.get(p).copied().flatten().is_some() && !drawn.contains(&p) {
            let w = usize::from(grid.width());
            return Err(PackError::BuildingOutsideMaterials {
                file: fname.to_owned(),
                x: i % w,
                y: i / w,
            });
        }
    }
    let glass = index(Material::Glass).and_then(|i| PaletteIndex::try_from(i).ok());
    let windows = glass.map_or_else(Vec::new, |g| runs_of(grid, g));
    Ok(BuildingArt {
        sprite: Sprite::new(Vec1::new(one), Arc::clone(palette), 0, None),
        windows,
    })
}

/// The one frame of `marked`, if it holds exactly one.
fn one_frame(marked: Vec<(IndexedFrame, Vec<Mark>)>) -> Option<(IndexedFrame, Vec<Mark>)> {
    let mut frames = marked.into_iter();
    match (frames.next(), frames.next()) {
        (Some(one), None) => Some(one),
        _ => None,
    }
}

/// Each 4-connected run of `index` in `grid`, as its pixels.
fn runs_of(grid: &Grid<PaletteIndex>, index: PaletteIndex) -> Vec<Vec<(u16, u16)>> {
    let (w, h) = (grid.width(), grid.height());
    let mut seen = vec![false; usize::from(w) * usize::from(h)];
    let mut runs = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let at = usize::from(y) * usize::from(w) + usize::from(x);
            if seen[at] || grid.get(x, y) != Some(&index) {
                continue;
            }
            seen[at] = true;
            let (mut run, mut stack) = (Vec::new(), vec![(x, y)]);
            while let Some((cx, cy)) = stack.pop() {
                run.push((cx, cy));
                let steps = [
                    (cx.checked_add(1), Some(cy)),
                    (cx.checked_sub(1), Some(cy)),
                    (Some(cx), cy.checked_add(1)),
                    (Some(cx), cy.checked_sub(1)),
                ];
                for (nx, ny) in steps {
                    let (Some(nx), Some(ny)) = (nx, ny) else {
                        continue;
                    };
                    if nx >= w || ny >= h {
                        continue;
                    }
                    let n = usize::from(ny) * usize::from(w) + usize::from(nx);
                    if !seen[n] && grid.get(nx, ny) == Some(&index) {
                        seen[n] = true;
                        stack.push((nx, ny));
                    }
                }
            }
            run.sort_unstable();
            runs.push(run);
        }
    }
    runs
}

/// The file a pack directory's manifest is read from.
pub(crate) const PACK_MANIFEST: &str = "pack.toml";

/// A `Pack` from in-memory strings: its manifest and each frame file it names.
///
/// # Errors
///
/// If `pack_toml` does not parse, a frame it names is absent from `frames`, or the pack fails validation.
#[cfg(feature = "pack-toml")]
pub fn load_pack_from_strings(pack_toml: &str, frames: &[(&str, &str)]) -> Result<Pack> {
    load_from_strings(pack_toml, frames, &mut |_| {})
}

/// A `Pack` from a manifest a build script transcoded to JSON, and each frame
/// file it names: what a build without `pack-toml` loads.
///
/// # Errors
///
/// If `pack_json` does not parse, a frame it names is absent from `frames`, or the pack fails validation.
pub fn load_pack_from_json(pack_json: &str, frames: &[(&str, &str)]) -> Result<Pack> {
    let parsed: PackToml =
        serde_json::from_str(pack_json).map_err(|e| PackError::JsonManifest {
            message: e.to_string(),
        })?;
    build_from_strings(parsed, frames, &mut |_| {})
}

/// The one-frame sprite [`fill_pack_manifest`] draws every piece it adds in.
#[cfg(feature = "pack-toml")]
pub(crate) const FILLER_SPRITE: (&str, &str) = ("filler.sprite", "@frame 0\n~");

/// The palette keys [`fill_pack_manifest`] adds: the filler's, then one per
/// [`Material`] and the character outline's.
#[cfg(feature = "pack-toml")]
const FILLER_KEYS: [char; 9] = ['~', '^', '`', '|', '{', '}', '<', '>', '='];

/// `pack_toml` with every piece it leaves out drawn in [`FILLER_SPRITE`],
/// each walk given a stride, and `[city]` and `[characters]` added where it
/// has none: a test's pack names only what it tests and still loads.
///
/// # Errors
///
/// If `pack_toml` is not TOML.
#[cfg(feature = "pack-toml")]
pub(crate) fn fill_pack_manifest(pack_toml: &str) -> Result<String> {
    use toml::{Table, Value};
    let mut manifest: Table = pack_toml
        .parse()
        .map_err(|source| PackError::Manifest { source })?;
    // Taken out and put back: a key that holds no table is a test's mistake
    // the load reports.
    let mut take = |key: &str| match manifest.remove(key) {
        Some(Value::Table(t)) => t,
        _ => Table::new(),
    };
    let (mut palette, mut animations) = (take("palette"), take("animations"));
    for (i, key) in FILLER_KEYS.into_iter().enumerate() {
        palette
            .entry(key.to_string())
            .or_insert_with(|| Value::String(format!("#0000{i:02x}")));
    }
    for &piece in Piece::VARIANTS {
        animations.entry(piece.name()).or_insert_with(|| {
            let mut anim = Table::new();
            let frames = vec![Value::String(FILLER_SPRITE.0.to_owned()); piece.min_frames()];
            anim.insert("frames".into(), Value::Array(frames));
            anim.insert("frame_ms".into(), Value::Integer(100));
            if piece.walk().is_some() {
                anim.insert("stride".into(), Value::Integer(1));
            }
            Value::Table(anim)
        });
    }
    manifest.insert("palette".into(), Value::Table(palette));
    manifest.insert("animations".into(), Value::Table(animations));
    if !manifest.contains_key("city") {
        let city = Material::ALL
            .iter()
            .zip(&FILLER_KEYS[1..])
            .map(|(m, k)| (m.name().to_owned(), Value::String(k.to_string())))
            .collect();
        manifest.insert("city".into(), Value::Table(city));
    }
    if !manifest.contains_key("characters") {
        let mut characters = Table::new();
        characters.insert("outline".into(), Value::String(FILLER_KEYS[8].to_string()));
        manifest.insert("characters".into(), Value::Table(characters));
    }
    Ok(manifest.to_string())
}

/// [`load_pack_from_strings`] of [`fill_pack_manifest`]`(pack_toml)`, with
/// [`FILLER_SPRITE`] among `frames`.
///
/// # Errors
///
/// As [`load_pack_from_strings`].
#[cfg(feature = "pack-toml")]
#[doc(hidden)]
pub fn load_filled_pack(pack_toml: &str, frames: &[(&str, &str)]) -> Result<Pack> {
    let frames: Vec<_> = frames.iter().copied().chain([FILLER_SPRITE]).collect();
    load_pack_from_strings(&fill_pack_manifest(pack_toml)?, &frames)
}

/// The frame files [`load_pack_from_strings`] reads, in the order it reads
/// them: every one it reads it needs, so a file missing from them is one the
/// manifest never draws.
///
/// # Errors
///
/// As [`load_pack_from_strings`].
#[cfg(feature = "pack-toml")]
#[doc(hidden)]
pub fn frames_read_by(pack_toml: &str, frames: &[(&str, &str)]) -> Result<Vec<String>> {
    let mut read = Vec::new();
    load_from_strings(pack_toml, frames, &mut |fname| read.push(fname.to_owned()))?;
    Ok(read)
}

#[cfg(feature = "pack-toml")]
fn load_from_strings(
    pack_toml: &str,
    frames: &[(&str, &str)],
    on_read: &mut dyn FnMut(&str),
) -> Result<Pack> {
    let parsed: PackToml =
        toml::from_str(pack_toml).map_err(|source| PackError::Manifest { source })?;
    build_from_strings(parsed, frames, on_read)
}

fn build_from_strings(
    parsed: PackToml,
    frames: &[(&str, &str)],
    on_read: &mut dyn FnMut(&str),
) -> Result<Pack> {
    let frame_lookup: HashMap<&str, &str> = frames.iter().copied().collect();

    build_pack(parsed, &mut |fname| {
        on_read(fname);
        frame_lookup
            .get(fname)
            .map(|s| s.to_string())
            .ok_or_else(|| PackError::MissingEmbeddedFrame {
                file: fname.to_owned(),
            })
    })
}

/// Frame file `file`'s source, decoded against `palette`.
fn decode(file: &str, src: &str, palette: &Palette) -> Result<Vec<(IndexedFrame, Vec<Mark>)>> {
    parse_indexed(src, palette).map_err(|source| PackError::Decode {
        file: file.to_owned(),
        source,
    })
}

fn single_char(k: &str, what: KeySite) -> Result<char> {
    let mut it = k.chars();
    let (Some(key), None) = (it.next(), it.next()) else {
        return Err(PackError::NotOneChar {
            what,
            key: k.to_owned(),
        });
    };
    Ok(key)
}

/// The furthest a `[ramps]` level may step either way. Past it the darkest
/// colors stop changing from one level to the next in 8 bits. Also the depth
/// [`Rgb::ramp`](super::Rgb::ramp) shares its headroom over, so changing it recolours every ramp.
pub const MAX_RAMP_LEVEL: i8 = 10;

fn build_palette(
    colors: &BTreeMap<String, String>,
    ramps: &BTreeMap<String, RampToml>,
) -> Result<Palette> {
    let keys = colors.len() + ramps.len();
    if keys > PALETTE_CAPACITY {
        return Err(PackError::PaletteTooLarge { keys });
    }
    let mut palette = Palette::new();
    for (k, v) in colors {
        let key = single_char(k, KeySite::Palette)?;
        let pixel = parse_palette_value(v).map_err(|source| PackError::Color {
            key: k.clone(),
            source,
        })?;
        palette.insert(key, pixel);
    }
    for (k, ramp) in ramps {
        let key = single_char(k, KeySite::Ramp)?;
        let of = single_char(&ramp.of, KeySite::RampBase)?;
        if colors.contains_key(k) {
            return Err(PackError::PaletteAndRamp { key });
        }
        // `colors`, not the palette being built, names the possible bases, so
        // an earlier-loaded ramp is never one.
        if !colors.contains_key(&ramp.of) || !matches!(palette.get(of), Some(Some(_))) {
            return Err(PackError::RampBase { key, of });
        }
        // Zero is a second name for the base itself.
        if ramp.level == 0 || ramp.level.unsigned_abs() > MAX_RAMP_LEVEL.unsigned_abs() {
            return Err(PackError::RampLevel {
                key,
                level: ramp.level,
            });
        }
        palette.insert_ramp(key, of, ramp.level);
    }
    Ok(palette)
}

/// Every piece a pack draws: the character poses, the furniture and the
/// room's fixtures, and the creatures. A pack ships each, so a loaded
/// [`Pack`] hands out any of them ([`Pack::piece`]); the manifest keys it
/// by its [`name`](Self::name).
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

/// What a [`Piece`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceKind {
    /// A character pose, which a hairstyle dresses.
    Character,
    /// Furniture or a fixture of the room.
    Furniture,
    /// A pet or a gateway mascot: it stands on its feet wherever its frame
    /// ends, so it is kept apart from the furniture.
    Creature,
}

impl Piece {
    /// Its key in the manifest's `[animations]`.
    pub fn name(self) -> &'static str {
        self.into()
    }

    /// The piece `name` keys, if it keys one.
    pub fn from_name(name: &str) -> Option<Self> {
        name.parse().ok()
    }

    /// What it is.
    pub fn kind(self) -> PieceKind {
        use Piece::*;
        match self {
            Seated | Typing | Standing | Walking | WalkingBack | SeatedSleeping
            | SeatedSleepingAlt | HoldingCoffee | BackCouch | WalkingCoffee | SideSeated
            | SeatedBack | TypingBack => PieceKind::Character,
            CatWalk | CatSit | CatSleep | DogWalk | DogSit | DogSleep | LobsterWalk
            | LobsterRest => PieceKind::Creature,
            _ => PieceKind::Furniture,
        }
    }

    /// The walk it is, if a walker steps it by the ground it covers.
    pub fn walk(self) -> Option<Walk> {
        Walk::VARIANTS.iter().copied().find(|w| w.piece() == self)
    }

    /// The fewest frames it is drawn in: an animation that never moves is
    /// not one.
    pub fn min_frames(self) -> usize {
        match self {
            Piece::Door => 3,
            Piece::Typing
            | Piece::Walking
            | Piece::WalkingBack
            | Piece::CatWalk
            | Piece::DogWalk
            | Piece::LobsterWalk => 2,
            _ => 1,
        }
    }

    /// The piece it is drawn over, on that piece's canvas: `desk_front` is
    /// what of `desk` stands nearer the viewer than its sitter's props. An
    /// overlay keeps that piece's canvas rather than grounding on its own
    /// bottom row.
    pub fn overlay_of(self) -> Option<Piece> {
        match self {
            Piece::DeskFront => Some(Piece::Desk),
            _ => None,
        }
    }
}

/// A piece a walker steps by the ground it covers: its frames advance by
/// distance, so a planted foot stays planted at any speed ([`Pack::stride`]).
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
fn parse_key(name: &str) -> Option<(Piece, Option<Density>)> {
    match split_density_variant(name) {
        Some((base, density)) => Some((Piece::from_name(base)?, Some(density))),
        None => Some((Piece::from_name(name)?, None)),
    }
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
    #[doc(hidden)]
    pub const fn as_nonzero(self) -> NonZeroU16 {
        self.0
    }
}

impl std::fmt::Display for Density {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The animation name for `base` drawn at `density`x.
pub fn density_variant_name(base: &str, density: Density) -> String {
    let mut out = String::with_capacity(base.len() + 4);
    density_variant_name_into(&mut out, base, density);
    out
}

/// [`density_variant_name`] into a caller-owned buffer, so a lookup loop reuses
/// one allocation.
pub(crate) fn density_variant_name_into(out: &mut String, base: &str, density: Density) {
    use std::fmt::Write;
    // Writing to a String is infallible.
    let _ = write!(out, "{base}{DENSITY_VARIANT_SEP}{density}x");
}

/// The largest density a variant name may claim.
///
/// A pack author types this number, so a claim past any real authoring grid is
/// a typo, and a bound there costs no real pack anything. It keeps
/// `desk@60000x` an unknown name rather than a variant, which would become one
/// of [`Pack::density_variants`] that no real render scale lands on.
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
mod validation_floor_tests {
    use super::*;

    fn d(n: u16) -> Density {
        Density::new(n).expect("nonzero")
    }

    /// A pack of `animations` over the filler, whose frame files are `frames`.
    fn try_pack_with_frames(animations: &str, frames: &[(&str, &str)]) -> Result<Pack> {
        load_filled_pack(
            &format!("[palette]\n\"A\"=\"#010203\"\n{animations}"),
            frames,
        )
    }

    fn pack_with_frames(animations: &str, frames: &[(&str, &str)]) -> Pack {
        try_pack_with_frames(animations, frames).expect("pack builds")
    }

    fn pack_with(animations: &str) -> Pack {
        pack_with_frames(animations, &[("f.sprite", "@frame 0\nA")])
    }

    /// `animations` as a manifest `try_pack_with_frames` refuses.
    fn refused(animations: &str, frames: &[(&str, &str)]) -> PackError {
        try_pack_with_frames(animations, frames).expect_err("the pack does not load")
    }

    /// `[animations.<key>]` of `files`, 100 ms a frame.
    fn anim(key: &str, files: &[&str]) -> String {
        let files: Vec<_> = files.iter().map(|f| format!("\"{f}\"")).collect();
        format!(
            "[animations.\"{key}\"]\nframes=[{}]\nframe_ms=100\n",
            files.join(", ")
        )
    }

    /// Each frame [`frames_read_by`] reports is one the pack fails to load
    /// without, and each other is one it loads without: the read set is the
    /// needed set a leave-one-out load would find.
    #[test]
    fn the_frames_read_are_the_frames_needed() {
        let toml = fill_pack_manifest(
            "[palette]\n\"A\"=\"#010203\"\n\
             [animations.typing]\nframes=[\"a.sprite\", \"b.sprite\"]\nframe_ms=100\n",
        )
        .expect("a manifest");
        let frames = [
            ("a.sprite", "@frame 0\nA"),
            ("b.sprite", "@frame 0\nA"),
            ("stray.sprite", "@frame 0\nA"),
            FILLER_SPRITE,
        ];
        let read = frames_read_by(&toml, &frames).expect("the whole set loads");
        assert!(!read.iter().any(|r| r == "stray.sprite"));
        for (name, _) in frames {
            let without: Vec<_> = frames.iter().copied().filter(|&(n, _)| n != name).collect();
            assert_eq!(
                read.iter().any(|r| r == name),
                load_pack_from_strings(&toml, &without).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn the_filler_loads_an_empty_manifest() {
        let pack = load_filled_pack("[palette]\n", &[]).expect("every piece is filled in");
        for &piece in Piece::VARIANTS {
            assert!(
                pack.piece(piece).frames().len() >= piece.min_frames(),
                "{piece:?}"
            );
            assert!(pack.variants_of(piece).is_empty(), "{piece:?}");
        }
        for &walk in Walk::VARIANTS {
            assert_eq!(pack.stride(walk).get(), 1, "{walk:?}");
        }
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

    #[test]
    fn sprite_first_is_its_first_frame() {
        let pack = pack_with_frames(&anim("typing", &["one.sprite", "two.sprite"]), SIZED_FRAMES);
        let typing = pack.piece(Piece::Typing);
        assert!(std::ptr::eq(typing.first(), &typing.frames()[0]));
        assert_eq!(typing.first().width(), 1);
        assert_eq!(typing.frames()[1].width(), 2);
    }

    #[test]
    fn a_pack_lacking_a_piece_does_not_load() {
        use toml::{Table, Value};
        let full: Table = fill_pack_manifest("[palette]\n")
            .expect("a manifest")
            .parse()
            .expect("toml");
        for &piece in Piece::VARIANTS {
            let mut manifest = full.clone();
            let Some(Value::Table(animations)) = manifest.get_mut("animations") else {
                panic!("the filled manifest has animations");
            };
            animations.remove(piece.name());
            let err = load_pack_from_strings(&manifest.to_string(), &[FILLER_SPRITE])
                .expect_err("a piece is missing");
            assert!(
                matches!(err, PackError::MissingPiece { piece: p } if p == piece),
                "{piece:?}: {err:?}"
            );
        }
    }

    #[test]
    fn an_unknown_animation_does_not_load() {
        for key in ["idle", "dsek@4x", "desk@1x", "typo@64x", "desk@60000x"] {
            let err = refused(&anim(key, &["f.sprite"]), &[("f.sprite", "@frame 0\nA")]);
            assert!(
                matches!(&err, PackError::UnknownAnimation { key: k } if k == key),
                "{key}: {err:?}"
            );
        }
    }

    #[test]
    fn a_pack_without_city_or_characters_does_not_load() {
        for table in ["city", "characters"] {
            let mut manifest: toml::Table = fill_pack_manifest("[palette]\n")
                .expect("a manifest")
                .parse()
                .expect("toml");
            manifest.remove(table);
            let err = load_pack_from_strings(&manifest.to_string(), &[FILLER_SPRITE])
                .expect_err("a required table is missing");
            assert!(
                matches!(&err, PackError::Manifest { source } if source.message().contains(table)),
                "{table}: {err:?}"
            );
        }
    }

    /// A manifest's JSON transcode loads, and one the parser or the schema
    /// refuses is a `JsonManifest` error that says why.
    #[test]
    fn a_json_manifest_loads_and_a_bad_one_says_why() {
        let manifest = fill_pack_manifest("[palette]\n").expect("a manifest");
        let json: serde_json::Value = toml::from_str(&manifest).expect("toml");
        load_pack_from_json(&json.to_string(), &[FILLER_SPRITE]).expect("the transcode loads");
        let mut unknown = json.clone();
        unknown["sprites"] = serde_json::json!({});
        for (bad, why) in [("{".to_owned(), "EOF"), (unknown.to_string(), "sprites")] {
            let err = load_pack_from_json(&bad, &[FILLER_SPRITE]).expect_err("refused");
            assert!(
                matches!(&err, PackError::JsonManifest { message } if message.contains(why)),
                "{why}: {err:?}"
            );
        }
    }

    /// A walk's frames advance by its stride, so one without it does not load.
    #[test]
    fn a_walk_without_a_stride_does_not_load() {
        for &walk in Walk::VARIANTS {
            let piece = walk.piece();
            let files = vec!["f.sprite"; piece.min_frames()];
            let err = refused(&anim(piece.name(), &files), &[("f.sprite", "@frame 0\nA")]);
            assert!(
                matches!(err, PackError::WalkWithoutStride { walk: p } if p == piece),
                "{piece:?}: {err:?}"
            );
        }
    }

    #[test]
    fn a_short_animation_does_not_load() {
        let frame = [("f.sprite", "@frame 0\nA")];
        for &piece in Piece::VARIANTS {
            let stride = if piece.walk().is_some() {
                "stride=1\n"
            } else {
                ""
            };
            let load = |n: usize| {
                let files = vec!["f.sprite"; n];
                try_pack_with_frames(&format!("{}{stride}", anim(piece.name(), &files)), &frame)
            };
            assert!(load(piece.min_frames()).is_ok(), "{piece:?}");
            let err = load(piece.min_frames() - 1).expect_err("one frame short");
            assert!(
                matches!(
                    &err,
                    PackError::TooFewFrames { key, need, have }
                        if key == piece.name()
                            && *need == piece.min_frames()
                            && *have == piece.min_frames() - 1
                ),
                "{piece:?}: {err:?}"
            );
        }
    }

    /// A piece of `door`, `typing` and `seated` is each the fewest frames that
    /// registry row names.
    #[test]
    fn a_door_needs_three_frames_a_typist_two_and_any_other_piece_one() {
        for (name, need) in [("door", 3), ("typing", 2), ("seated", 1)] {
            let have = need - 1;
            let err = refused(
                &anim(name, &vec!["f.sprite"; have]),
                &[("f.sprite", "@frame 0\nA")],
            );
            assert!(
                matches!(&err, PackError::TooFewFrames { key, need: n, have: h }
                    if key == name && *n == need && *h == have),
                "{name}: {err:?}"
            );
        }
    }

    #[test]
    fn an_animation_with_no_frames_does_not_load() {
        for name in ["seated", "desk", "cat_sit"] {
            let err = refused(
                &format!("[animations.{name}]\nframes=[]\nframe_ms=100\n"),
                &[],
            );
            assert!(
                matches!(&err, PackError::TooFewFrames { key, need: 1, have: 0 } if key == name),
                "{name}: {err:?}"
            );
        }
    }

    /// Pins [`Pack::icon`]: each art `[icons]` names loads.
    #[test]
    fn an_icon_loads_the_art_it_names() {
        let pack = pack_with("[icons]\nstar=\"f.sprite\"\n");
        let star = pack.icon("star").expect("the star loads");
        assert_eq!(star.frames().len(), 1);
        assert_eq!(pack.icon_names().collect::<Vec<_>>(), ["star"]);
    }

    #[test]
    fn an_icon_of_two_frames_is_refused() {
        let err = refused(
            "[icons]\nstar=\"two.sprite\"\n",
            &[("two.sprite", "@frame 0\nA\n@frame 1\nA")],
        );
        assert!(matches!(err, PackError::IconFrames { file } if file == "two.sprite"));
    }

    /// A walk carries its stride; a stride of nothing is no walk and the pack
    /// refuses it.
    #[test]
    fn a_walk_carries_its_stride_and_refuses_a_zero_one() {
        let pack = pack_with(
            "[animations.walking]\nframes=[\"f.sprite\", \"f.sprite\"]\nframe_ms=100\nstride=12\n\
             [animations.seated]\nframes=[\"f.sprite\"]\nframe_ms=100\n",
        );
        assert_eq!(pack.stride(Walk::Walking).get(), 12);
        assert_eq!(
            pack.piece(Piece::Walking).stride().map(|s| s.get()),
            Some(12)
        );
        assert_eq!(pack.piece(Piece::Seated).stride(), None);
        let zero = try_pack_with_frames(
            "[animations.walking]\nframes=[\"f.sprite\", \"f.sprite\"]\nframe_ms=100\nstride=0\n",
            &[("f.sprite", "@frame 0\nA")],
        );
        assert!(zero.is_err(), "a zero stride loaded");
    }

    /// 1x1 and 3x1 base frames, their 2x variants and a 4x of the 1x1; each
    /// also misses every other's claim.
    const SIZED_FRAMES: &[(&str, &str)] = &[
        ("one.sprite", "@frame 0\nA"),
        ("two.sprite", "@frame 0\nA A\nA A"),
        ("three.sprite", "@frame 0\nA A A"),
        ("six.sprite", "@frame 0\nA A A A A A\nA A A A A A"),
        (
            "four.sprite",
            "@frame 0\nA A A A\nA A A A\nA A A A\nA A A A",
        ),
    ];

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

    /// Pins [`split_density_variant`]'s one spelling per density.
    #[test]
    fn a_density_with_a_leading_zero_is_not_a_variant() {
        assert_eq!(split_density_variant("desk@04x"), None);
        let err = refused(
            &format!(
                "{}{}",
                anim("desk", &["one.sprite"]),
                anim("desk@02x", &["two.sprite"])
            ),
            SIZED_FRAMES,
        );
        assert!(
            matches!(&err, PackError::UnknownAnimation { key } if key == "desk@02x"),
            "{err:?}"
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

    /// Pins [`Pack::density_variants`]: densest first, each density once.
    #[test]
    fn density_variants_are_the_densities_the_pack_redraws_densest_first() {
        let pack = pack_with_frames(
            "[animations.typing]\nframes=[\"one.sprite\", \"one.sprite\"]\nframe_ms=100\n\
             [animations.\"typing@2x\"]\nframes=[\"two.sprite\", \"two.sprite\"]\nframe_ms=100\n\
             [animations.\"typing@4x\"]\nframes=[\"four.sprite\", \"four.sprite\"]\nframe_ms=100\n\
             [animations.walking]\nframes=[\"one.sprite\", \"one.sprite\"]\nframe_ms=100\nstride=1\n\
             [animations.\"walking@2x\"]\nframes=[\"two.sprite\", \"two.sprite\"]\nframe_ms=100\n",
            SIZED_FRAMES,
        );
        assert_eq!(pack.density_variants(), vec![d(4), d(2)]);
        assert_eq!(pack.max_density_variant(), d(4));
        assert!(pack_with("").density_variants().is_empty());
    }

    /// Frame count and size are both part of redrawing a piece; either one
    /// wrong refuses the pack.
    #[test]
    fn a_variant_that_does_not_redraw_its_piece_does_not_load() {
        let typing = |variant: &[&str]| {
            format!(
                "{}{}",
                anim("typing", &["one.sprite", "one.sprite"]),
                anim("typing@2x", variant)
            )
        };
        for (variant, variant_frames, why) in [
            (
                &["two.sprite", "three.sprite"][..],
                2,
                "a frame the wrong size",
            ),
            (&["two.sprite"], 1, "a frame short"),
            (&["three.sprite"], 1, "a frame short and the wrong size"),
            (
                &["two.sprite", "two.sprite", "two.sprite"],
                3,
                "a frame over",
            ),
        ] {
            let err = refused(&typing(variant), SIZED_FRAMES);
            assert!(
                matches!(&err, PackError::VariantDoesNotRedraw {
                    key, base_frames: 2, variant_frames: v
                } if key == "typing@2x" && *v == variant_frames),
                "{why}: {err:?}"
            );
        }
        assert!(try_pack_with_frames(&typing(&["two.sprite", "two.sprite"]), SIZED_FRAMES).is_ok());
    }

    /// Pins the MATCHING base frame, in [`variant_redraws`] and in load's
    /// refusal of a variant that fails it.
    #[test]
    fn each_variant_frame_is_proved_against_the_matching_base_frame() {
        let walking = |variant: &[&str]| {
            format!(
                "{}stride=1\n{}",
                anim("walking", &["one.sprite", "three.sprite"]),
                anim("walking@2x", variant)
            )
        };
        let pack = pack_with_frames(&walking(&["two.sprite", "six.sprite"]), SIZED_FRAMES);
        assert!(variant_redraws(
            pack.piece(Piece::Walking),
            d(2),
            &pack.variants_of(Piece::Walking)[&d(2)]
        ));
        // each frame fits a base frame, but not the matching one
        let err = refused(&walking(&["six.sprite", "two.sprite"]), SIZED_FRAMES);
        assert!(
            matches!(err, PackError::VariantDoesNotRedraw { .. }),
            "{err:?}"
        );
    }

    /// Pins [`DENSITY_VARIANT_SEP`]'s claim: `@4x` of a 2-wide base is 8 wide.
    #[test]
    fn a_variant_that_lies_about_its_density_does_not_load() {
        let err = refused(
            "[animations.desk]\nframes=[\"one.sprite\"]\nframe_ms=100\n\
             [animations.\"desk@4x\"]\nframes=[\"four.sprite\"]\nframe_ms=100\n",
            &[
                ("one.sprite", "@frame 0\nA A"),
                // 4 wide, not the 8 that `@4x` of a 2-wide base claims.
                ("four.sprite", "@frame 0\nA A A A"),
            ],
        );
        assert!(
            matches!(&err, PackError::VariantDoesNotRedraw { key, .. } if key == "desk@4x"),
            "{err:?}"
        );
    }

    /// Pins the empty case of the variant's frame count.
    #[test]
    fn an_empty_density_variant_does_not_load() {
        let err = refused(
            "[animations.desk]\nframes=[\"f.sprite\"]\nframe_ms=100\n\
             [animations.\"desk@4x\"]\nframes=[]\nframe_ms=100\n",
            &[("f.sprite", "@frame 0\nA")],
        );
        assert!(
            matches!(&err, PackError::TooFewFrames { key, need: 1, have: 0 } if key == "desk@4x"),
            "{err:?}"
        );
    }

    /// Pins [`Pack::max_density_variant`].
    #[test]
    fn the_packs_max_density_is_the_scale_a_painter_has_to_round_to() {
        let pack = |extra: &str| {
            try_pack_with_frames(
                &format!(
                    "[animations.desk]\nframes=[\"one.sprite\"]\nframe_ms=100\n\
                     [animations.\"desk@2x\"]\nframes=[\"two.sprite\"]\nframe_ms=100\n\
                     [animations.plant]\nframes=[\"one.sprite\"]\nframe_ms=100\n{extra}"
                ),
                SIZED_FRAMES,
            )
        };
        let plain = pack_with("");
        assert_eq!(
            plain.max_density_variant(),
            Density::ONE,
            "a pack with no variants must not push a painter off the cell's own scale"
        );
        assert_eq!(
            pack("[animations.\"plant@4x\"]\nframes=[\"four.sprite\"]\nframe_ms=100\n")
                .expect("loads")
                .max_density_variant(),
            d(4)
        );
        assert!(
            pack("[animations.\"typo@64x\"]\nframes=[\"one.sprite\"]\nframe_ms=100\n").is_err(),
            "a variant of an unregistered base must not inflate the pack's density"
        );
        assert!(
            pack("[animations.\"plant@8x\"]\nframes=[\"two.sprite\"]\nframe_ms=100\n").is_err(),
            "a variant no renderer could draw must not round a painter's scale to it"
        );
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

    /// Pins [`claimed_variant_size`].
    #[test]
    fn a_claim_no_frame_can_meet_does_not_load() {
        let row = |w: usize| format!("{}\n", "A ".repeat(w).trim_end());
        let base = format!("@frame 0\n{}", row(40_000));
        let variant = format!("@frame 0\n{0}{0}", row(u16::MAX as usize));
        let err = refused(
            "[animations.desk]\nframes=[\"base.sprite\"]\nframe_ms=100\n\
             [animations.\"desk@2x\"]\nframes=[\"variant.sprite\"]\nframe_ms=100\n",
            &[("base.sprite", &base), ("variant.sprite", &variant)],
        );
        assert!(
            matches!(&err, PackError::VariantDoesNotRedraw { key, .. } if key == "desk@2x"),
            "80_000 wide is claimed, 65_535 is found: {err:?}"
        );
    }
}

fn parse_palette_value(v: &str) -> Result<Pixel, ColorError> {
    if v.eq_ignore_ascii_case("transparent") {
        return Ok(None);
    }
    let not_hex = || ColorError::Hex {
        value: v.to_owned(),
    };
    let hex = v.strip_prefix('#').ok_or_else(|| ColorError::Prefix {
        value: v.to_owned(),
    })?;
    // `u8::from_str_radix` accepts a leading '+', so `#+f0102` would slice to
    // `+f`/`01`/`02` and parse as a valid color without this explicit hex check.
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(not_hex());
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| not_hex());
    let (r, g, b) = (channel(0)?, channel(2)?, channel(4)?);
    Ok(Some(Rgb { r, g, b }))
}
