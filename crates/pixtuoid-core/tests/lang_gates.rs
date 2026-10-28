//! Every plugin the installer ships has its contract row
//! (`install/plugin-contract.json`), its selftest, and the host API it is typed
//! against. Which sources a checker reads at all is `just lang-census`'s, from
//! the checkers themselves. Reads the tree at runtime, so it is workspace-only
//! and sits in `Cargo.toml`'s `exclude`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const PLUGIN_DIR: &str = "crates/pixtuoid/src/install";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Every tracked path, as git spells it.
fn tracked() -> Vec<String> {
    let out = Command::new("git")
        .current_dir(root())
        .args(["ls-files", "-z"])
        .output()
        .expect("git ls-files");
    assert!(out.status.success(), "git ls-files failed");
    out.stdout
        .split(|&b| b == 0)
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect()
}

#[test]
fn every_shipped_plugin_has_its_contract_row_and_its_selftest() {
    let tracked = tracked();
    let contract: Value =
        serde_json::from_str(&read(&format!("{PLUGIN_DIR}/plugin-contract.json")))
            .expect("the contract");
    let rows: BTreeSet<&str> = contract["plugins"]
        .as_array()
        .expect("plugins")
        .iter()
        .map(|p| p["file"].as_str().expect("a file"))
        .collect();
    let pyproject: toml::Table = read("pyproject.toml").parse().expect("pyproject.toml");
    let strict: Vec<&str> = pyproject["tool"]["pyright"]["strict"]
        .as_array()
        .expect("[tool.pyright] strict")
        .iter()
        .map(|p| p.as_str().expect("a path"))
        .collect();
    let mut plugins = BTreeSet::new();
    for path in &tracked {
        let Some(file) = path.strip_prefix(&format!("{PLUGIN_DIR}/")) else {
            continue;
        };
        let Some((stem, ext)) = file.rsplit_once('.').filter(|_| !file.contains('/')) else {
            continue;
        };
        let python = match ext {
            "py" => true,
            "ts" | "js" | "mjs" if !stem.ends_with(".d") => false,
            _ => continue,
        };
        plugins.insert(file);
        let selftest = format!(
            "scripts/{}.test.{}",
            stem.replace('_', "-"),
            if python { "py" } else { "mjs" }
        );
        assert!(
            tracked.contains(&selftest),
            "{path} ships with no selftest at {selftest}: it is how a plugin is executed against \
             plugin-contract.json"
        );
        assert!(
            !python || strict.contains(&PLUGIN_DIR) || strict.contains(&path.as_str()),
            "{path} ships, so pyproject.toml's [tool.pyright] names it or {PLUGIN_DIR} in `strict`"
        );
    }
    assert_eq!(
        plugins, rows,
        "every shipped plugin, and only those, has a row in plugin-contract.json \
         (`install/contract.rs`); `just plugin-types` checks it against its host"
    );
    for row in contract["plugins"].as_array().expect("plugins") {
        let source = row["source"].as_str().expect("a source");
        let published = pixtuoid_core::harness::hermetic::profile(source)
            .is_some_and(|p| !p.plugin_types.is_empty());
        let declared = ["d.ts", "pyi"]
            .iter()
            .any(|ext| tracked.contains(&format!("{PLUGIN_DIR}/{source}_host.{ext}")));
        assert!(
            published || declared,
            "{source}'s plugin is typed against no host API: name the npm packages its host \
             publishes in its profile's plugin_types, or declare what it uses in {PLUGIN_DIR}/{source}_host.*"
        );
    }
}
