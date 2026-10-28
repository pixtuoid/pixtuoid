//! Bundle the default pack: the sprite list is generated from `sprites/default/`
//! itself, so a sprite committed there is bundled by construction, less,
//! without the `cutaway-assets` feature, what
//! [`density_art::bundled_without_density_art`] drops from both the manifest
//! and the list. Every bundled file goes in without its comments
//! ([`comments::strip_comments`]).

#[path = "build_support/comments.rs"]
mod comments;
#[path = "build_support/density_art.rs"]
mod density_art;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let manifest_dir =
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set");
    let out_dir = std::env::var_os("OUT_DIR").expect("OUT_DIR must be set");
    let out_dir = Path::new(&out_dir);
    let asset_dir = Path::new(&manifest_dir).join("sprites/default");

    // The one rerun trigger: cargo rescans a directory for any change, so an
    // added, removed or edited file there, `pack.toml` included, reruns this
    // script, which rewrites everything it embeds.
    println!("cargo:rerun-if-changed={}", asset_dir.display());

    // Core's `PACK_MANIFEST`, which a build script cannot import: a rename fails
    // this read, and so the build.
    let pack_toml = std::fs::read_to_string(asset_dir.join("pack.toml")).expect("read pack.toml");
    let (pack_toml, dropped) = if std::env::var_os("CARGO_FEATURE_CUTAWAY_ASSETS").is_some() {
        (comments::strip_comments(&pack_toml), BTreeSet::new())
    } else {
        density_art::bundled_without_density_art(&pack_toml)
    };
    // For `load_pack_from_json`; the tests edit the TOML.
    let pack_json: serde_json::Value = toml::from_str(&pack_toml).expect("pack.toml is TOML");
    std::fs::write(out_dir.join("bundled_pack.json"), pack_json.to_string())
        .expect("write bundled_pack.json");
    std::fs::write(out_dir.join("bundled_pack.toml"), pack_toml).expect("write bundled_pack.toml");
    let stripped = out_dir.join("sprites");
    std::fs::create_dir_all(&stripped).expect("create the stripped sprite dir");

    let mut sprites: Vec<_> = std::fs::read_dir(&asset_dir)
        .expect("read sprites/default")
        .map(|entry| entry.expect("read a sprites/default entry").path())
        // A dotfile is never a sprite, whatever its extension: macOS writes
        // `._<name>` AppleDouble files beside real ones on non-HFS volumes.
        .filter(|path| {
            path.extension().is_some_and(|e| e == "sprite")
                && !path
                    .file_name()
                    .is_some_and(|n| n.as_encoded_bytes().starts_with(b"."))
        })
        .collect();
    // Sorted, so the generated list — and the binary — do not depend on the
    // order the filesystem happens to list the directory in.
    sprites.sort();

    let mut list = String::from("&[\n");
    for path in &sprites {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("a sprite file name is UTF-8");
        if dropped.contains(name) {
            continue;
        }
        let src = std::fs::read_to_string(path).expect("read a sprite");
        let bundled = stripped.join(name);
        std::fs::write(&bundled, comments::strip_comments(&src)).expect("write a stripped sprite");
        // `{:?}` escapes the path for a Rust string literal, backslashes
        // included.
        writeln!(
            list,
            "    ({name:?}, include_str!({:?})),",
            bundled.display().to_string()
        )
        .expect("writing to a String");
    }
    list.push_str("]\n");
    std::fs::write(out_dir.join("bundled_sprites.rs"), list).expect("write bundled_sprites.rs");
}
