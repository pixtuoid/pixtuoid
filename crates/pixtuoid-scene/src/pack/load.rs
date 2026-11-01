//! Loading a [`Pack`]: the manifest's schema and the build that checks it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU16;
use std::sync::Arc;

use enum_map::EnumMap;
use serde::Deserialize;
#[cfg(test)]
use strum::VariantArray;
use vec1::Vec1;

use super::error::{ColorError, KeySite, PackError, Repr};
use super::piece::{Piece, Walk, parse_key};
use super::{
    Building, BuildingArt, CityMaterials, CityPlane, HairLayers, Hairstyle, Material, Pack,
};
use crate::grid::Grid;
use crate::sprite::format::{
    DENSITY_VARIANT_SEP, Density, parse_indexed, split_density_variant, variant_redraws,
};
use crate::sprite::{
    HeadMark, HeadView, IndexedFrame, MAX_RAMP_LEVEL, Mark, PALETTE_CAPACITY, Palette,
    PaletteIndex, Pixel, Rgb, Sprite,
};

type Result<T, E = Repr> = std::result::Result<T, E>;

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
            return Err(Repr::CityUnknownMaterial {
                name: other.clone(),
            });
        }
        let mut keys = [' '; Material::ALL.len()];
        for material in Material::ALL {
            let Some(key) = c.get(material.name()) else {
                return Err(Repr::CityMissingMaterial { material });
            };
            let what = KeySite::CityMaterial;
            let key = single_char(key, what)?;
            if !matches!(palette.get(key), Some(Some(_))) {
                return Err(Repr::NotOpaque { what, key });
            }
            if keys[..material.index()].contains(&key) {
                return Err(Repr::CityKeyShared { key });
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
            return Err(Repr::BuildingDensity { key });
        }
        let art = building_art(&building.sprite, &palette, materials, get_src)?;
        match (density, building.planes) {
            (Density::ONE, Some(planes)) if !planes.is_empty() => {
                let planes = planes
                    .iter()
                    .map(|p| {
                        CityPlane::from_name(p).ok_or_else(|| Repr::BuildingPlane {
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
            (Density::ONE, _) => return Err(Repr::BuildingWithoutPlanes { key }),
            (_, Some(_)) => return Err(Repr::VariantPlanes { key }),
            (_, None) => {
                let no_base = |key| Repr::VariantWithoutBase {
                    base: name.to_owned(),
                    key,
                };
                let Some(base) = buildings.get_mut(name) else {
                    return Err(no_base(key.clone()));
                };
                if !variant_redraws(&base.base.sprite, density, &art.sprite) {
                    let (base_w, base_h) = base.size();
                    return Err(Repr::VariantSize {
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
            _ => return Err(Repr::NotOpaque { what, key }),
        }
    };
    let mut hairstyles = BTreeMap::new();
    for (key, style) in parsed.hairstyles {
        let Some((name, density)) = split_density_variant(&key) else {
            return Err(Repr::HairstyleDensity { key });
        };
        let mut layer = |view: HeadView, fname: &str| -> Result<Sprite> {
            let src = get_src(fname)?;
            let Some(frame) = one_frame(decode(fname, &src, &palette)?) else {
                return Err(Repr::HairLayerFrames {
                    file: fname.to_owned(),
                });
            };
            if frame.1.iter().find_map(HeadMark::of).map(|h| h.view) != Some(view) {
                return Err(Repr::HairLayerHead {
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
        return Err(Repr::HairstylesDiffer {
            first: *first,
            other: *d,
        });
    }

    let mut icons = BTreeMap::new();
    for (name, fname) in parsed.icons {
        let src = get_src(&fname)?;
        let Some(frame) = one_frame(decode(&fname, &src, &palette)?) else {
            return Err(Repr::IconFrames { file: fname });
        };
        let art = Sprite::new(Vec1::new(frame), Arc::clone(&palette), 0, None);
        icons.insert(name, art);
    }

    let mut pack = Pack {
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
            return Err(Repr::UnknownAnimation { key });
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
            _ => return Err(Repr::TooFewFrames { key, need, have }),
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
    let pieces =
        EnumMap::try_from_fn(|piece| bases.remove(&piece).ok_or(Repr::MissingPiece { piece }))?;
    let strides = EnumMap::try_from_fn(|walk: Walk| {
        pieces[walk.piece()]
            .stride()
            .ok_or(Repr::WalkWithoutStride { walk: walk.piece() })
    })?;
    let mut redrawn: EnumMap<Piece, BTreeMap<Density, Sprite>> = EnumMap::default();
    for ((piece, density), (key, variant)) in variants {
        let base = &pieces[piece];
        if !variant_redraws(base, density, &variant) {
            return Err(Repr::VariantDoesNotRedraw {
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
        return Err(Repr::BuildingFrames {
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
            return Err(Repr::BuildingOutsideMaterials {
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

/// A `Pack` from a manifest a build script transcoded to JSON, and each frame
/// file it names.
///
/// # Errors
///
/// If `pack_json` does not parse, a frame it names is absent from `frames`, or the pack fails validation.
pub(crate) fn load_pack_from_json(
    pack_json: &str,
    frames: &[(&str, &str)],
) -> Result<Pack, PackError> {
    Ok(build_pack(
        manifest_from_json(pack_json)?,
        &mut frame_lookup(frames),
    )?)
}

fn manifest_from_json(pack_json: &str) -> Result<PackToml> {
    serde_json::from_str(pack_json).map_err(|e| Repr::JsonManifest {
        message: e.to_string(),
    })
}

/// The frame file `frames` holds under each name a manifest gives it.
fn frame_lookup(frames: &[(&str, &str)]) -> impl Fn(&str) -> Result<String> {
    let frames: HashMap<&str, &str> = frames.iter().copied().collect();
    move |fname| {
        frames
            .get(fname)
            .map(|s| s.to_string())
            .ok_or_else(|| Repr::MissingEmbeddedFrame {
                file: fname.to_owned(),
            })
    }
}

/// A manifest the tests write in TOML, as the JSON the loader reads.
#[cfg(test)]
fn transcode(pack_toml: &str) -> String {
    let manifest: serde_json::Value = toml::from_str(pack_toml).expect("a test's manifest is TOML");
    manifest.to_string()
}

/// [`load_pack_from_json`] of a TOML manifest's [`transcode`], with the error it hides.
#[cfg(test)]
pub(crate) fn load_pack_from_strings(pack_toml: &str, frames: &[(&str, &str)]) -> Result<Pack> {
    build_pack(
        manifest_from_json(&transcode(pack_toml))?,
        &mut frame_lookup(frames),
    )
}

/// The one-frame sprite [`fill_pack_manifest`] draws every piece it adds in.
#[cfg(test)]
pub(crate) const FILLER_SPRITE: (&str, &str) = ("filler.sprite", "@frame 0\n~");

/// The palette keys [`fill_pack_manifest`] adds: the filler's, then one per
/// [`Material`] and the character outline's.
#[cfg(test)]
const FILLER_KEYS: [char; 9] = ['~', '^', '`', '|', '{', '}', '<', '>', '='];

/// `pack_toml` with every piece it leaves out drawn in [`FILLER_SPRITE`],
/// each walk given a stride, and `[city]` and `[characters]` added where it
/// has none: a test's pack names only what it tests and still loads.
#[cfg(test)]
pub(crate) fn fill_pack_manifest(pack_toml: &str) -> String {
    use toml::{Table, Value};
    let mut manifest: Table = pack_toml.parse().expect("a test's manifest is TOML");
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
    manifest.to_string()
}

/// [`load_pack_from_strings`] of [`fill_pack_manifest`]`(pack_toml)`, with
/// [`FILLER_SPRITE`] among `frames`.
#[cfg(test)]
pub(crate) fn load_filled_pack(pack_toml: &str, frames: &[(&str, &str)]) -> Result<Pack> {
    let frames: Vec<_> = frames.iter().copied().chain([FILLER_SPRITE]).collect();
    load_pack_from_strings(&fill_pack_manifest(pack_toml), &frames)
}

/// The frame files [`load_pack_from_strings`] reads, in the order it reads
/// them: every one it reads it needs, so a file missing from them is one the
/// manifest never draws.
///
/// # Errors
///
/// As [`load_pack_from_strings`].
#[cfg(test)]
pub(crate) fn frames_read_by(pack_toml: &str, frames: &[(&str, &str)]) -> Result<Vec<String>> {
    let lookup = frame_lookup(frames);
    let mut read = Vec::new();
    build_pack(manifest_from_json(&transcode(pack_toml))?, &mut |fname| {
        read.push(fname.to_owned());
        lookup(fname)
    })?;
    Ok(read)
}

/// Frame file `file`'s source, decoded against `palette`.
fn decode(file: &str, src: &str, palette: &Palette) -> Result<Vec<(IndexedFrame, Vec<Mark>)>> {
    parse_indexed(src, palette).map_err(|source| Repr::Decode {
        file: file.to_owned(),
        source,
    })
}

fn single_char(k: &str, what: KeySite) -> Result<char> {
    let mut it = k.chars();
    let (Some(key), None) = (it.next(), it.next()) else {
        return Err(Repr::NotOneChar {
            what,
            key: k.to_owned(),
        });
    };
    Ok(key)
}

fn build_palette(
    colors: &BTreeMap<String, String>,
    ramps: &BTreeMap<String, RampToml>,
) -> Result<Palette> {
    let keys = colors.len() + ramps.len();
    if keys > PALETTE_CAPACITY {
        return Err(Repr::PaletteTooLarge { keys });
    }
    let mut palette = Palette::new();
    for (k, v) in colors {
        let key = single_char(k, KeySite::Palette)?;
        let pixel = parse_palette_value(v).map_err(|source| Repr::Color {
            key: k.clone(),
            source,
        })?;
        palette.insert(key, pixel);
    }
    for (k, ramp) in ramps {
        let key = single_char(k, KeySite::Ramp)?;
        let of = single_char(&ramp.of, KeySite::RampBase)?;
        if colors.contains_key(k) {
            return Err(Repr::PaletteAndRamp { key });
        }
        // `colors`, not the palette being built, names the possible bases, so
        // an earlier-loaded ramp is never one.
        if !colors.contains_key(&ramp.of) || !matches!(palette.get(of), Some(Some(_))) {
            return Err(Repr::RampBase { key, of });
        }
        // Zero is a second name for the base itself.
        if ramp.level == 0 || ramp.level.unsigned_abs() > MAX_RAMP_LEVEL.unsigned_abs() {
            return Err(Repr::RampLevel {
                key,
                level: ramp.level,
            });
        }
        palette.insert_ramp(key, of, ramp.level);
    }
    Ok(palette)
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

#[cfg(test)]
mod tests;
