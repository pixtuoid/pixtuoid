//! Every tracked source in a language the repo gates, outside the projects that
//! gate themselves, sits under its checker's include; and every plugin the
//! installer ships has its contract row (`install/plugin-contract.json`) and its
//! selftest. Reads the tree at runtime, so it is workspace-only and sits in
//! `Cargo.toml`'s `exclude`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// They run their own `tsc` and lint (`site/AGENTS.md`, `integrations/raycast/AGENTS.md`).
const SELF_GATED: [&str; 2] = ["site/", "integrations/"];
const PLUGIN_DIR: &str = "crates/pixtuoid/src/install";

#[derive(Debug, Clone, Copy, PartialEq)]
enum Lang {
    Python,
    Script,
    Shell,
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Every tracked path but the self-gated projects', as git spells it.
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
        .filter(|path| !SELF_GATED.iter().any(|dir| path.starts_with(dir)))
        .collect()
}

/// A path's language, by its extension, else by its shebang.
fn lang(path: &str) -> Option<Lang> {
    match Path::new(path).extension().and_then(|e| e.to_str()) {
        Some("py" | "pyi") => Some(Lang::Python),
        Some("js" | "mjs" | "cjs" | "ts" | "mts" | "cts") => Some(Lang::Script),
        Some("sh") => Some(Lang::Shell),
        Some(_) => None,
        // A tracked symlink to a directory (`.agents/skills/*`) is no source.
        None if !root().join(path).is_file() => None,
        None => {
            let first = read(path).lines().next().unwrap_or_default().to_owned();
            let interpreter = first
                .strip_prefix("#!")?
                .split_whitespace()
                .last()?
                .to_owned();
            match interpreter.rsplit('/').next()? {
                "node" => Some(Lang::Script),
                i if i.starts_with("python") => Some(Lang::Python),
                "sh" | "bash" => Some(Lang::Shell),
                _ => None,
            }
        }
    }
}

/// Whether `pattern`'s one `*` (if any) matches all of `name`. Neither a `*`
/// nor a directory's expansion reaches a hidden name: tsc's wildcards skip one,
/// and pyright excludes `**/.*` by default.
fn segment_matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => {
            (!name.starts_with('.') || head.starts_with('.'))
                && name.len() >= head.len() + tail.len()
                && name.starts_with(head)
                && name.ends_with(tail)
        }
    }
}

/// Whether `pattern`, relative to `base`, includes `path`: a directory includes
/// everything under it, as tsc's and pyright's `include` read one.
fn includes(base: &str, pattern: &str, path: &str) -> bool {
    let full: Vec<&str> = base
        .split('/')
        .chain(pattern.split('/'))
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        full.iter()
            .all(|s| *s != "**" && !s.contains(['?', '[', '{']) && s.matches('*').count() <= 1),
        "the census reads one `*` per segment, not {pattern:?}: widen `segment_matches`"
    );
    let parts: Vec<&str> = path.split('/').collect();
    parts.len() >= full.len()
        && full.iter().zip(&parts).all(|(p, s)| segment_matches(p, s))
        && !parts[full.len()..].iter().any(|s| s.starts_with('.'))
}

/// `tsconfig`'s include patterns, relative to its directory.
fn ts_includes(tsconfig: &str) -> (String, Vec<String>) {
    let config: Value = serde_json::from_str(&read(tsconfig)).expect("a JSON tsconfig");
    let dir = tsconfig
        .rsplit_once('/')
        .map_or("", |(dir, _)| dir)
        .to_owned();
    let patterns = config["include"].as_array().expect("an include list");
    (
        dir,
        patterns
            .iter()
            .map(|p| p.as_str().expect("a pattern").to_owned())
            .collect(),
    )
}

fn pyright() -> toml::Table {
    let pyproject: toml::Table = read("pyproject.toml").parse().expect("pyproject.toml");
    pyproject["tool"]["pyright"]
        .as_table()
        .expect("[tool.pyright]")
        .clone()
}

fn strings(value: Option<&toml::Value>) -> Vec<String> {
    value
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .map(|v| v.as_str().expect("a path").to_owned())
        .collect()
}

/// The checker each language's files must sit under, by the config that names them.
fn covered(lang: Lang, path: &str) -> bool {
    match lang {
        Lang::Python => strings(pyright().get("include"))
            .iter()
            .any(|p| includes("", p, path)),
        Lang::Script => ["tsconfig.json", &format!("{PLUGIN_DIR}/tsconfig.json")]
            .iter()
            .any(|config| {
                let (dir, patterns) = ts_includes(config);
                patterns.iter().any(|p| includes(&dir, p, path))
            }),
        Lang::Shell => {
            let justfile = read("justfile");
            let sources = justfile
                .lines()
                .find_map(|line| line.strip_prefix("SHELL_SOURCES := "))
                .expect("the justfile's SHELL_SOURCES");
            sources
                .trim_matches('"')
                .split_whitespace()
                .any(|p| includes("", p, path))
        }
    }
}

#[test]
fn every_gated_source_is_under_its_checkers_include() {
    let uncovered: Vec<String> = tracked()
        .into_iter()
        .filter_map(|path| Some((lang(&path)?, path)))
        .filter(|(lang, path)| !covered(*lang, path))
        .map(|(lang, path)| format!("{path} ({lang:?})"))
        .collect();
    assert!(
        uncovered.is_empty(),
        "no checker includes {uncovered:?}: add each to pyproject.toml's [tool.pyright], a \
         tsconfig.json's include, or the justfile's SHELL_SOURCES; a script with no extension \
         gets one, since neither tsc nor pyright includes an extensionless file"
    );
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
    let strict = strings(pyright().get("strict"));
    let mut plugins = BTreeSet::new();
    for path in &tracked {
        let Some(file) = path.strip_prefix(&format!("{PLUGIN_DIR}/")) else {
            continue;
        };
        let Some(lang) = lang(path) else { continue };
        if file.contains('/') || file.ends_with(".d.ts") || file.ends_with(".pyi") {
            continue;
        }
        plugins.insert(file);
        let (stem, _) = file.rsplit_once('.').expect("an extension");
        let selftest = format!(
            "scripts/{}.test.{}",
            stem.replace('_', "-"),
            if lang == Lang::Python { "py" } else { "mjs" }
        );
        assert!(
            tracked.contains(&selftest),
            "{path} ships with no selftest at {selftest}: it is how a plugin is executed against \
             plugin-contract.json"
        );
        if lang == Lang::Python {
            assert!(
                strict.iter().any(|p| includes("", p, path)),
                "{path} ships, so pyproject.toml's [tool.pyright] checks it `strict`"
            );
        }
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

#[test]
fn the_census_reads_includes_as_its_checkers_do() {
    assert!(includes("", "scripts", "scripts/lib/plugin-harness.mjs"));
    assert!(includes(
        PLUGIN_DIR,
        "*.ts",
        "crates/pixtuoid/src/install/omp_extension.ts"
    ));
    assert!(!includes(
        PLUGIN_DIR,
        "*.ts",
        "crates/pixtuoid/src/install/hook_cmd/x.ts"
    ));
    assert!(!includes(
        "",
        "scripts/lib/*.sh",
        "scripts/lib/capture-omp-tui.py"
    ));
    assert!(!includes("", "npm", "npmx/generate.mjs"));
    assert!(!includes("", "hermetic", "hermetic/codex/.codex/deny.js"));
    assert!(!includes("", "hermetic/*/*.js", "hermetic/.codex/deny.js"));
    assert!(includes(
        "",
        "hermetic/*/.codex/*.js",
        "hermetic/p/.codex/deny.js"
    ));
    assert_eq!(lang("npm/pixtuoid/bin/pixtuoid.js"), Some(Lang::Script));
    assert_eq!(lang(".githooks/pre-push"), Some(Lang::Shell));
    assert_eq!(lang("justfile"), None);
}
