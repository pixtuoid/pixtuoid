//! The site's manifests, which a test holds a registry's names to: site CI never
//! runs the binary, so such a test is the only bridge between the two.

use std::path::Path;

use serde_json::Value;

/// `file` under `site/src/` as a JSON list. `None` in a packaged crate, which
/// ships no `site/`; a file missing from a tree that has one fails instead.
pub(crate) fn manifest(file: &str) -> Option<Vec<Value>> {
    manifest_in(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../site"),
        file,
    )
}

fn manifest_in(site: &Path, file: &str) -> Option<Vec<Value>> {
    if !site.is_dir() {
        eprintln!("skipping: {} not present (packaged build)", site.display());
        return None;
    }
    let path = site.join("src").join(file);
    let json = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    Some(serde_json::from_str(&json).unwrap_or_else(|e| panic!("{path:?}: {e}")))
}

#[test]
fn a_tree_with_no_site_skips() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(manifest_in(&dir.path().join("site"), "themes.json"), None);
}

#[test]
#[should_panic(expected = "themes.json")]
fn a_manifest_missing_from_a_present_site_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _ = manifest_in(dir.path(), "themes.json");
}

#[test]
fn a_present_manifest_reads_as_its_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("src")).expect("src");
    std::fs::write(dir.path().join("src/themes.json"), r#"[{"id":"a"}]"#).expect("write");
    let list = manifest_in(dir.path(), "themes.json").expect("a site");
    assert_eq!(list[0]["id"], "a");
}
