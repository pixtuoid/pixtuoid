//! A crates.io tarball holds only its own crate's directory, plus what
//! `cargo package` copies in. MIT makes its text a condition of every copy, so
//! each published crate's `LICENSE` is a symlink to the root one, which
//! `cargo package` flattens to its target
//! (<https://doc.rust-lang.org/cargo/commands/cargo-package.html>). Its
//! crates.io page is the `readme` it inherits from the workspace root
//! (<https://doc.rust-lang.org/cargo/reference/workspaces.html>), which
//! `cargo package` copies into the crate's root.

// Unix only: a Windows checkout without `core.symlinks` writes the link as a
// file holding its target path, and the crates publish from release-plz.yml's
// ubuntu job, where the link is real.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn cargo(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO"))
        .args(args)
        .arg("--offline")
        .current_dir(root())
        .output()
        .expect("run cargo");
    assert!(
        out.status.success(),
        "cargo {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("cargo writes UTF-8")
}

struct Published {
    name: String,
    dir: PathBuf,
    /// As cargo resolves it, so an inherited path is the workspace root's.
    readme: Option<PathBuf>,
}

fn published() -> Vec<Published> {
    let meta: serde_json::Value =
        serde_json::from_str(&cargo(&["metadata", "--no-deps", "--format-version", "1"]))
            .expect("parse cargo metadata");
    let crates: Vec<Published> = meta["packages"]
        .as_array()
        .expect("metadata packages")
        .iter()
        // `publish = false` reads as an empty registry list.
        .filter(|p| p["publish"].as_array().is_none_or(|r| !r.is_empty()))
        .map(|p| {
            let dir = Path::new(p["manifest_path"].as_str().expect("manifest path"))
                .parent()
                .expect("manifest dir")
                .to_path_buf();
            Published {
                name: p["name"].as_str().expect("package name").to_owned(),
                readme: p["readme"].as_str().map(|r| dir.join(r)),
                dir,
            }
        })
        .collect();
    assert!(!crates.is_empty(), "the workspace publishes no crate");
    crates
}

/// The package list too: an `include`/`exclude` edit drops a present file.
fn packages(name: &str, file: &str) -> bool {
    cargo(&["package", "--list", "--allow-dirty", "-p", name])
        .lines()
        .any(|l| l == file)
}

#[test]
fn every_published_crate_carries_the_root_license_text() {
    let license = std::fs::read_to_string(root().join("LICENSE")).expect("read the root LICENSE");
    for krate in published() {
        let name = &krate.name;
        let text = std::fs::read_to_string(krate.dir.join("LICENSE")).ok();
        assert!(
            text.as_deref() == Some(license.as_str()),
            "{name}'s LICENSE is not the root's text"
        );
        assert!(
            packages(name, "LICENSE"),
            "{name} would publish without its LICENSE"
        );
    }
}

/// crates.io resolves a README's relative link against the publishing crate's
/// directory in the repository, the tarball's `path_in_vcs`
/// (<https://github.com/rust-lang/crates.io/blob/dba69e3d0891d4f3604465da9355ce88dfc547c8/crates/crates_io_markdown/src/lib.rs#L308-L319>),
/// so a README shared from the root breaks every relative link and image.
#[test]
fn every_published_crate_carries_a_readme_crates_io_renders_whole() {
    for krate in published() {
        let name = &krate.name;
        let readme = krate
            .readme
            .unwrap_or_else(|| panic!("{name} has no `readme`, so its crates.io page is blank"));
        assert!(
            packages(name, "README.md"),
            "{name} would publish without its README"
        );
        let text = std::fs::read_to_string(&readme)
            .unwrap_or_else(|e| panic!("read {name}'s readme {readme:?}: {e}"));
        let relative: Vec<&str> = link_targets(&text)
            .into_iter()
            .filter(|t| !is_absolute(t))
            .collect();
        assert!(
            relative.is_empty(),
            "{name}'s readme {readme:?} links {relative:?} relatively"
        );
    }
}

/// Every link target in a Markdown file: an inline `](target)`, a reference
/// definition's `]: target`, and raw HTML's `href`, `src` and each `srcset`
/// candidate.
fn link_targets(text: &str) -> Vec<&str> {
    let mut targets: Vec<&str> = between(text, "](", ')')
        .chain(between(text, "href=\"", '"'))
        .chain(between(text, "src=\"", '"'))
        .map(first_word)
        .collect();
    targets.extend(between(text, "srcset=\"", '"').flat_map(|set| set.split(',').map(first_word)));
    targets.extend(
        text.lines()
            .filter(|l| l.trim_start().starts_with('['))
            .filter_map(|l| l.split_once("]: "))
            .map(|(_, target)| first_word(target)),
    );
    targets
}

fn between<'a>(text: &'a str, open: &'static str, close: char) -> impl Iterator<Item = &'a str> {
    text.match_indices(open).map(move |(at, _)| {
        let rest = &text[at + open.len()..];
        &rest[..rest.find(close).unwrap_or(rest.len())]
    })
}

fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or(s)
}

/// A fragment stays on the page; anything else must name its scheme (RFC 3986
/// §3.1: a letter, then letters, digits, `+`, `-` or `.`).
fn is_absolute(target: &str) -> bool {
    target.starts_with('#')
        || target.split_once(':').is_some_and(|(scheme, _)| {
            scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        })
}
