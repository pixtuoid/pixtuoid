//! MIT makes its text a condition of every copy, and a crates.io tarball holds
//! only its own crate's directory: each published crate's `LICENSE` is a
//! symlink to the root one, which `cargo package` flattens to its target
//! (<https://doc.rust-lang.org/cargo/commands/cargo-package.html>).

// Unix only: a Windows checkout without `core.symlinks` writes the link as a
// file holding its target path, and the crates publish from release-plz.yml's
// ubuntu job, where the link is real.
#![cfg(unix)]

use std::path::Path;

fn manifest(dir: &Path) -> toml::Table {
    std::fs::read_to_string(dir.join("Cargo.toml"))
        .expect("read manifest")
        .parse()
        .expect("parse manifest")
}

#[test]
fn every_published_crate_carries_the_root_license_text() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let license = std::fs::read_to_string(root.join("LICENSE")).expect("read the root LICENSE");
    let workspace = manifest(&root);
    let members = workspace["workspace"]["members"]
        .as_array()
        .expect("workspace members");
    let published: Vec<&str> = members
        .iter()
        .map(|m| m.as_str().expect("member path"))
        .filter(|m| {
            manifest(&root.join(m))["package"]
                .get("publish")
                .and_then(toml::Value::as_bool)
                != Some(false)
        })
        .collect();
    assert!(!published.is_empty(), "the workspace publishes no crate");
    for member in published {
        let text = std::fs::read_to_string(root.join(member).join("LICENSE")).ok();
        assert!(
            text.as_deref() == Some(license.as_str()),
            "{member} would publish without the LICENSE text"
        );
    }
}
