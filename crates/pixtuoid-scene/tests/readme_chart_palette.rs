//! Pins `scripts/readme-palette.json` — the office colours the README's star
//! chart, Buy Me a Coffee button and banner are painted with — to the theme
//! each variant names, by struct access. Their renderers are Python and can only
//! copy; this is the copy's guard, the same shape as `site_badge_colors.rs`.
//!
//! Reads the JSON at RUNTIME because `include_str!` of a path outside the
//! crate fails `cargo test` on the extracted .crate (no workspace tree).
//! Workspace-only test, excluded from the published package (`Cargo.toml`
//! `exclude`).

use pixtuoid_scene::sprite::Rgb;
use pixtuoid_scene::theme::{Theme, theme_by_name};

const PALETTE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/readme-palette.json"
);

fn hex(rgb: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b)
}

/// Every field the renderer paints with, and the theme role it copies.
fn roles(t: &Theme) -> [(&'static str, Rgb); 19] {
    [
        ("wall", t.surface.wall),
        ("trim", t.surface.wall_trim),
        ("carpet_light", t.surface.carpet_light),
        ("carpet_dark", t.surface.carpet_dark),
        ("title", t.ui.tooltip_title),
        ("text", t.ui.tooltip_text),
        ("star", t.lighting.desk_lamp),
        ("window_frame", t.surface.window_frame),
        ("sky_top", t.lighting.night_sky_b),
        ("sky_horizon", t.lighting.night_sky_a),
        ("building_dark", t.office.building_dark),
        ("building_light", t.office.building_light),
        ("city_dark_window", t.office.city_dark_window),
        ("city_lit_a", t.office.city_lit_windows[0]),
        ("city_lit_b", t.office.city_lit_windows[1]),
        ("city_lit_c", t.office.city_lit_windows[2]),
        ("moon", t.lighting.moon_core),
        ("neon_panel", t.office.neon_panel_bg),
        ("neon_brand", t.ui.neon_brand),
    ]
}

#[test]
fn readme_chart_palette_matches_the_named_themes_verbatim() {
    let text = std::fs::read_to_string(PALETTE_PATH)
        .unwrap_or_else(|e| panic!("read {PALETTE_PATH}: {e}"));
    let variants = serde_json::from_str::<serde_json::Value>(&text)
        .expect("readme-palette.json is valid JSON");
    let variants = variants
        .as_object()
        .expect("readme-palette.json is a JSON object keyed by README variant");

    let mut checked = 0usize;
    for (variant, row) in variants {
        let name = row["theme"]
            .as_str()
            .unwrap_or_else(|| panic!("variant {variant:?} has no string `theme`"));
        let theme = theme_by_name(name)
            .unwrap_or_else(|| panic!("variant {variant:?} names unregistered theme {name:?}"));
        for (field, rgb) in roles(theme) {
            let got = row[field]
                .as_str()
                .unwrap_or_else(|| panic!("variant {variant:?} has no `{field}`"));
            assert_eq!(
                got,
                hex(rgb),
                "readme-palette.json {variant}.{field} drifted from theme {name:?}"
            );
            checked += 1;
        }
        let extra: Vec<_> = row
            .as_object()
            .expect("variant row is an object")
            .keys()
            .filter(|k| *k != "theme" && !roles(theme).iter().any(|(f, _)| f == k))
            .collect();
        assert!(
            extra.is_empty(),
            "variant {variant:?} carries fields no theme role backs: {extra:?}"
        );
    }
    assert!(checked > 0, "no variants found — palette read failed?");
}
