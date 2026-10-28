//! Drop the bundled pack's density art — its `@Nx` animations, the hairstyles
//! that dress them and its buildings' `@Nx` variants — for the test base
//! [`base_pack_srcs`](super::base_pack_srcs).

use std::collections::BTreeSet;

/// `pack_toml` without its density-variant animations, its hairstyles (which
/// only ever dress density art) and its buildings' density variants, and the
/// files only those drew.
///
/// Every `@` key in the bundled manifest's `[animations]` and `[buildings]` is a
/// density variant: core's load refuses an animation key that names no
/// `Piece`, and a building base named with one. So core's
/// `DENSITY_VARIANT_SEP` alone identifies them here, where its
/// `split_density_variant` is crate-private.
pub(crate) fn strip_density_art(pack_toml: &str) -> (String, BTreeSet<String>) {
    let mut doc: toml_edit::DocumentMut = pack_toml.parse().expect("the bundled pack.toml parses");
    let mut dropped: BTreeSet<String> = BTreeSet::new();
    if let Some(styles) = doc.remove("hairstyles") {
        for (_, style) in styles.as_table_like().into_iter().flat_map(|t| t.iter()) {
            for (_, layers) in style.as_table_like().into_iter().flat_map(|t| t.iter()) {
                for (_, file) in layers.as_table_like().into_iter().flat_map(|t| t.iter()) {
                    dropped.extend(file.as_str().map(str::to_owned));
                }
            }
        }
    }
    if let Some(buildings) = doc.get_mut("buildings").and_then(|b| b.as_table_like_mut()) {
        let variants: Vec<String> = buildings
            .iter()
            .map(|(name, _)| name.to_string())
            .filter(|name| name.contains('@'))
            .collect();
        for name in &variants {
            let sprite = buildings.get(name).and_then(|b| b.get("sprite"));
            dropped.extend(sprite.and_then(|f| f.as_str()).map(str::to_owned));
            buildings.remove(name);
        }
    }
    let animations = doc["animations"]
        .as_table_like_mut()
        .expect("the bundled pack.toml has an [animations] table");
    let variants: Vec<String> = animations
        .iter()
        .map(|(name, _)| name.to_string())
        .filter(|name| name.contains('@'))
        .collect();
    let frames_of = |item: Option<&toml_edit::Item>| -> Vec<String> {
        item.and_then(|a| a.get("frames"))
            .and_then(|f| f.as_array())
            .map(|f| {
                f.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    for name in &variants {
        dropped.extend(frames_of(animations.get(name)));
        animations.remove(name);
    }
    // A frame a kept animation still draws stays, even if a variant shared it.
    for (_, kept) in animations.iter() {
        for frame in frames_of(Some(kept)) {
            dropped.remove(&frame);
        }
    }
    (doc.to_string(), dropped)
}

#[cfg(test)]
mod tests {
    use super::strip_density_art;

    #[test]
    fn a_building_keeps_its_base_and_loses_its_density_variants() {
        let (toml, dropped) = strip_density_art(
            "[animations.a]\nframes = [\"a.sprite\"]\n\
             [buildings.tower]\nsprite = \"t.sprite\"\nplanes = [\"near\"]\n\
             [buildings.\"tower@4x\"]\nsprite = \"t@4x.sprite\"\n",
        );
        assert_eq!(dropped.into_iter().collect::<Vec<_>>(), ["t@4x.sprite"]);
        assert!(
            toml.contains("[buildings.tower]") && !toml.contains("tower@4x"),
            "{toml}"
        );
    }

    #[test]
    fn hairstyles_go_with_the_density_art_they_dress() {
        let (toml, dropped) = strip_density_art(
            "[animations.a]\nframes = [\"a.sprite\"]\n\
             [hairstyles.\"mop@2x\"]\nfront = { behind = \"b.sprite\", over = \"o.sprite\" }\n",
        );
        assert_eq!(
            dropped.into_iter().collect::<Vec<_>>(),
            ["b.sprite", "o.sprite"]
        );
        assert!(!toml.contains("hairstyles"), "{toml}");
    }

    #[test]
    fn a_frame_a_kept_animation_draws_survives_its_variant() {
        let (toml, dropped) = strip_density_art(
            "[animations.a]\nframes = [\"a.sprite\"]\n\
             [animations.\"a@2x\"]\nframes = [\"a.sprite\", \"a@2x.sprite\"]\n",
        );
        assert_eq!(dropped.into_iter().collect::<Vec<_>>(), ["a@2x.sprite"]);
        assert!(
            toml.contains("[animations.a]") && !toml.contains("a@2x"),
            "{toml}"
        );
    }
}
