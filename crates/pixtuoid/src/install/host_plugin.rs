//! The plugin pixtuoid lays out for a CLI whose own plugin system loads the hooks:
//! a root per host under pixtuoid's config dir, so no two hosts share a file, holding
//! one plugin named [`NAME`] that the host's CLI registers.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::install::io;
use crate::install::target::MergeOutcome;
use crate::install::verify::{SchemaParse, ShimRef, assemble};

/// The marketplace and the plugin share one name, so the plugin id is
/// `pixtuoid@pixtuoid`.
pub(crate) const NAME: &str = "pixtuoid";

/// A hooks file's place in its plugin dir: Claude Code's default, and Codex's
/// (`DEFAULT_HOOKS_CONFIG_FILE` in `codex-rs/core-plugins/src/loader.rs` @ rust-v0.162.1).
pub(crate) const HOOKS_FILE: &[&str] = &["hooks", "hooks.json"];

/// Where a plugin file keeps its per-event hooks, and where an event's first entry
/// keeps its command, as JSON pointers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HooksAt {
    pub(crate) hooks: &'static str,
    pub(crate) command: &'static str,
}

/// Claude Code's plugin shape, `{"hooks": {<event>: [{"hooks": [{"command": …}]}]}}`,
/// which Codex's plugins read too (`codex-rs/config/src/hook_config.rs` @ rust-v0.162.1).
pub(crate) const CLAUDE_SHAPE: HooksAt = HooksAt {
    hooks: "/hooks",
    command: "/hooks/0/command",
};

pub(crate) fn plugin_id() -> String {
    format!("{NAME}@{NAME}")
}

/// `<pixtuoid config>/<root>/<plugin>/<file…>`.
pub(crate) fn owned_path(root: &str, file: &[&str]) -> Result<PathBuf> {
    let base = crate::config::config_base().context("no home directory resolves")?;
    Ok(file
        .iter()
        .fold(base.join("pixtuoid").join(root).join(NAME), |p, c| {
            p.join(c)
        }))
}

/// The plugin dir and the root of `<root>/<plugin>/<file…>`, `file` as [`owned_path`]
/// joined it.
pub(crate) fn layout<'a>(config: &'a Path, file: &[&str]) -> Result<(&'a Path, &'a Path)> {
    let plugin = config.ancestors().nth(file.len());
    plugin.zip(plugin.and_then(Path::parent)).with_context(|| {
        format!(
            "{} is not <root>/<plugin>/{}",
            config.display(),
            file.join("/")
        )
    })
}

/// `./<plugin dir name>`, how a marketplace names a plugin dir inside its root.
pub(crate) fn relative_source(plugin: &Path) -> Result<String> {
    let name = plugin.file_name().context("the plugin dir has no name")?;
    Ok(format!("./{}", name.to_string_lossy()))
}

/// Write each manifest, creating its dir.
pub(crate) fn write_manifests(docs: &[(PathBuf, Value)]) -> Result<()> {
    for (path, doc) in docs {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        io::write_config_atomic(path, &serde_json::to_string_pretty(doc)?)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Delete `dir` only when `manifest` names [`NAME`]: a custom config path must not
/// take a stranger's dir.
pub(crate) fn remove_if_ours(dir: &Path, manifest: &Path) -> Result<()> {
    let Ok(text) = std::fs::read_to_string(manifest) else {
        return Ok(());
    };
    let ours = serde_json::from_str::<Value>(&text).is_ok_and(|m| m["name"].as_str() == Some(NAME));
    if ours {
        std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    Ok(())
}

/// `{<event>: [entry]}` for each of `events`.
pub(crate) fn hooks_map(events: &[&str], entry: &Value) -> Value {
    events
        .iter()
        .map(|ev| ((*ev).to_string(), json!([entry])))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// A hooks file in [`CLAUDE_SHAPE`], one `group` per event.
pub(crate) fn render_hooks(events: &[&str], group: &Value) -> Result<String> {
    Ok(serde_json::to_string_pretty(
        &json!({ "hooks": hooks_map(events, group) }),
    )?)
}

/// Every event in `events` holds an entry at `at`, and the first one's command gives
/// the shim back through `shim_of`.
pub(crate) fn verify_hooks(
    content: &str,
    at: HooksAt,
    events: &[&str],
    shim_of: fn(command: &str) -> ShimRef,
) -> SchemaParse {
    let Ok(doc) = serde_json::from_str::<Value>(content) else {
        return SchemaParse::broken("the plugin file no longer parses as JSON");
    };
    let hooks = doc.pointer(at.hooks).and_then(Value::as_object);
    let mut missing = Vec::new();
    let mut any = false;
    let mut shim = ShimRef::Unknown;
    for ev in events {
        let entry = hooks
            .and_then(|h| h.get(*ev))
            .and_then(Value::as_array)
            .and_then(|entries| entries.first());
        let Some(entry) = entry else {
            missing.push(*ev);
            continue;
        };
        any = true;
        if shim == ShimRef::Unknown {
            shim = entry
                .pointer(at.command)
                .and_then(Value::as_str)
                .map_or(ShimRef::Unknown, shim_of);
        }
    }
    // `assemble`'s own wording names a sentinel no plugin file carries.
    if !any {
        return SchemaParse::broken(
            "the plugin file holds no pixtuoid hooks (hand-edited, or emptied by a \
             disconnect) — reconnect via the Sources panel",
        );
    }
    assemble(&missing, any, shim, vec![])
}

/// Whether `content` holds any hooks at `at`, which a disconnect then removes.
pub(crate) fn holds_hooks(content: &str, at: HooksAt) -> bool {
    serde_json::from_str::<Value>(content)
        .ok()
        .and_then(|doc| doc.pointer(at.hooks)?.as_object().map(|h| !h.is_empty()))
        .unwrap_or(false)
}

/// A [`CLAUDE_SHAPE`] hooks file as a disconnect leaves it.
pub(crate) fn uninstall_hooks(content: &str) -> Result<MergeOutcome> {
    Ok(MergeOutcome {
        content: "{}\n".to_string(),
        changed: holds_hooks(content, CLAUDE_SHAPE),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hooks_file_sits_two_levels_inside_its_plugin_root() {
        let base = std::env::temp_dir().join("pixtuoid-xdg");
        temp_env::with_var("XDG_CONFIG_HOME", Some(&base), || {
            let hooks = owned_path("some-plugin", HOOKS_FILE).unwrap();
            let root = base.join("pixtuoid").join("some-plugin");
            assert_eq!(
                hooks,
                root.join("pixtuoid").join("hooks").join("hooks.json")
            );
            let (plugin, at) = layout(&hooks, HOOKS_FILE).unwrap();
            assert_eq!(
                (plugin, at),
                (root.join("pixtuoid").as_path(), root.as_path())
            );
            assert_eq!(relative_source(plugin).unwrap(), "./pixtuoid");
        });
    }

    #[test]
    fn uninstall_empties_the_file_and_reports_a_change_only_with_hooks() {
        let installed = render_hooks(&["Stop"], &json!({ "hooks": [] })).unwrap();
        let out = uninstall_hooks(&installed).unwrap();
        assert!(out.changed);
        assert!(!uninstall_hooks(&out.content).unwrap().changed);
        assert!(!uninstall_hooks("").unwrap().changed);
    }

    /// A plugin file carries no sentinel, so one with no hooks must not be reported
    /// as missing it.
    #[test]
    fn a_plugin_file_with_no_hooks_says_so_without_naming_a_sentinel() {
        for content in ["{}", r#"{"hooks":{}}"#] {
            let v = verify_hooks(content, CLAUDE_SHAPE, &["Stop"], |_| ShimRef::Unknown);
            assert_eq!(v.issues.len(), 1, "{content}: {:?}", v.issues);
            assert!(
                v.issues[0].contains("no pixtuoid hooks")
                    && !v.issues[0].contains("sentinel")
                    && !v.issues[0].contains("_pixtuoid"),
                "{content}: {:?}",
                v.issues
            );
        }
    }

    #[test]
    fn only_a_manifest_naming_us_lets_its_dir_go() {
        let tmp = tempfile::TempDir::new().unwrap();
        let manifest = tmp.path().join("dir").join("m.json");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, r#"{"name":"someone-else"}"#).unwrap();
        remove_if_ours(manifest.parent().unwrap(), &manifest).unwrap();
        assert!(manifest.exists());
        std::fs::write(&manifest, r#"{"name":"pixtuoid"}"#).unwrap();
        remove_if_ours(manifest.parent().unwrap(), &manifest).unwrap();
        assert!(!manifest.exists());
    }
}
