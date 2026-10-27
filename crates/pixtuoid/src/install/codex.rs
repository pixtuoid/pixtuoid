use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;

use crate::install::SENTINEL_KEY;
use crate::install::host_plugin::{self, NAME};
use crate::install::io;
use crate::install::target::{
    HostRegistration, LegacyConfig, MergeOutcome, Pickup, PluginCopy, Unregistered,
};
use crate::install::verify::{SchemaParse, ShimRef};

pub(crate) const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "UserPromptSubmit",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "PermissionRequest",
    // Fires at GRACEFUL teardown only → an immediate clean exit walk; abrupt
    // exits still ride the probe ladder + short-idle reap.
    "SessionEnd",
];

/// The `codex plugin` subcommands that add and remove one.
const PLUGIN_ADD: &str = "add";
const PLUGIN_REMOVE: &str = "remove";
/// The `codex plugin marketplace` subcommands that add and remove one: a
/// separate upstream enum.
const MARKETPLACE_ADD: &str = "add";
const MARKETPLACE_REMOVE: &str = "remove";
/// The `config.toml` tables and keys a marketplace and a plugin are recorded in.
const MARKETPLACES: &str = "marketplaces";
const SOURCE: &str = "source";
const PLUGINS: &str = "plugins";
const ENABLED: &str = "enabled";
/// Where in a marketplace root Codex reads its manifest.
const MARKETPLACE_MANIFEST: &str = ".agents/plugins/marketplace.json";
/// Where under Codex's home it caches an installed plugin.
const PLUGINS_CACHE: &str = "plugins/cache";
/// The version directory a version-less plugin lands in.
const LOCAL_VERSION: &str = "local";

/// Codex loads the hooks through its plugin system: a local marketplace it records in
/// place, and a plugin `codex plugin add` copies into its cache.
pub(crate) const HOST: HostRegistration = HostRegistration {
    cli: CLI,
    register,
    unregister,
    is_registered,
    pickup: Some(Pickup::Recopy(PluginCopy {
        stale: copy_is_stale,
        refresh: add_plugin,
    })),
    legacy: Some(LegacyConfig {
        path: config_toml,
        strip: merge_uninstall,
    }),
};

const CLI: io::HostCli = io::HostCli { command: "codex" };

pub(crate) fn default_config_path() -> Result<PathBuf> {
    host_plugin::owned_path("codex-plugin", host_plugin::HOOKS_FILE)
}

/// Codex's own config, where it records the marketplace and the plugin's enablement.
/// The SAME codex_home() the watcher uses, so the two can't disagree.
fn config_toml() -> Result<PathBuf> {
    Ok(pixtuoid_core::source::codex::codex_home().join("config.toml"))
}

/// Codex is present when its config is: an empty ~/.codex says nothing.
pub(crate) fn detect_installed() -> bool {
    config_toml().is_ok_and(|p| crate::install::target::config_present(&p))
}

/// The Codex hook `command`. Codex runs it under a shell (`/bin/sh -lc` on Unix,
/// `cmd.exe /C` on Windows) and reads the plain `command` field on every OS, so the
/// OS-correct form is written here rather than a `commandWindows` override. The
/// forms themselves are [`crate::install::hook_cmd::shell_hook_command`]'s (#195).
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    // `_explicit` is Claude's bare-name-vs-absolute switch — Codex always
    // embeds the absolute path, so the flag changes nothing here.
    let p = crate::install::merge::hook_path_str(resolved)?;
    crate::install::hook_cmd::shell_hook_command(p, pixtuoid_core::source::codex::SOURCE_NAME)
}

pub(crate) fn render_hooks(hook_cmd: &str) -> Result<String> {
    // No `matcher`: an omitted matcher means "match all" in Codex. Do NOT write
    // `"*"` — codex rejects a bare `*` as an invalid regex and silently
    // drops the ENTIRE group, so SessionStart/PreToolUse never fire.
    let group = json!({
        "hooks": [{
            "type": "command",
            "command": hook_cmd,
            "timeout": 5,
            "statusMessage": "pixtuoid visualizer",
        }]
    });
    host_plugin::render_hooks(CODEX_EVENTS, &group)
}

pub(crate) fn verify_schema(content: &str) -> SchemaParse {
    host_plugin::verify_hooks(
        content,
        host_plugin::CLAUDE_SHAPE,
        CODEX_EVENTS,
        crate::install::verify::shell_shim_ref,
    )
}

pub(crate) fn installed_shim(content: &str) -> ShimRef {
    verify_schema(content).shim
}

fn marketplace_manifest(root: &Path) -> PathBuf {
    MARKETPLACE_MANIFEST
        .split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

fn register(config: &Path) -> Result<()> {
    let (plugin, root) = host_plugin::layout(config, host_plugin::HOOKS_FILE)?;
    host_plugin::write_manifests(&[
        (
            marketplace_manifest(root),
            json!({
                "name": NAME,
                "plugins": [{
                    "name": NAME,
                    "source": { "source": "local", "path": host_plugin::relative_source(plugin)? },
                }],
            }),
        ),
        (
            plugin.join(".codex-plugin").join("plugin.json"),
            // No `version`: Codex then copies the plugin to a `local` dir
            // (`codex-rs/core-plugin-common/src/installed.rs` @ rust-v0.162.1), so no
            // release moves what it runs.
            json!({
                "name": NAME,
                "description": "Sends Codex's session events to the pixtuoid office",
            }),
        ),
    ])?;
    // A repeat add of the same local source is a no-op, and of another source an
    // error (`codex-rs/core-plugins/src/marketplace_add.rs` @ rust-v0.162.1).
    if marketplace_moved_from(root)? {
        remove_marketplace()?;
    }
    CLI.run(&[
        "plugin".as_ref(),
        "marketplace".as_ref(),
        MARKETPLACE_ADD.as_ref(),
        root.as_os_str(),
    ])?;
    add_plugin()
}

/// Whether Codex records our marketplace from a dir other than `root`, as after
/// pixtuoid's config dir moved. `codex plugin marketplace add` records a local
/// source as `[marketplaces.<name>] source`, canonicalized
/// (`codex-rs/core-plugins/src/marketplace_add.rs:336-358` and
/// `marketplace_add/source.rs:149-163` @ rust-v0.162.1).
fn marketplace_moved_from(root: &Path) -> Result<bool> {
    let doc = read_codex_config()?;
    let Some(source) = doc
        .get(MARKETPLACES)
        .and_then(|m| m.get(NAME))
        .and_then(|m| m.get(SOURCE))
        .and_then(toml::Value::as_str)
    else {
        return Ok(false);
    };
    // A source gone from disk has moved.
    Ok(std::fs::canonicalize(source).ok() != Some(std::fs::canonicalize(root)?))
}

fn remove_marketplace() -> Result<()> {
    CLI.run(&[
        "plugin".as_ref(),
        "marketplace".as_ref(),
        MARKETPLACE_REMOVE.as_ref(),
        NAME.as_ref(),
    ])?;
    Ok(())
}

/// Copy the plugin into Codex's cache, replacing an older copy, and enable it
/// (`codex-rs/core-plugins/src/manager.rs` @ rust-v0.162.1).
fn add_plugin() -> Result<()> {
    CLI.run(&[
        "plugin".as_ref(),
        PLUGIN_ADD.as_ref(),
        host_plugin::plugin_id().as_ref(),
    ])?;
    Ok(())
}

/// Whether the hooks file Codex runs, under [`PLUGINS_CACHE`]`/<marketplace>/
/// <plugin>/`[`LOCAL_VERSION`] (`codex-rs/core-plugins/src/store.rs`
/// @ rust-v0.162.1), differs from ours at `config`. A missing copy differs.
fn copy_is_stale(config: &Path) -> Result<bool> {
    let copy = PLUGINS_CACHE
        .split('/')
        .chain([NAME, NAME, LOCAL_VERSION])
        .chain(host_plugin::HOOKS_FILE.iter().copied())
        .fold(pixtuoid_core::source::codex::codex_home(), |p, c| p.join(c));
    Ok(io::read_config(&copy)? != io::read_config(config)?)
}

/// Reads Codex's config first, so a disconnect with nothing registered starts no CLI.
fn unregister(config: &Path) -> Result<Unregistered> {
    let (_, root) = host_plugin::layout(config, host_plugin::HOOKS_FILE)?;
    let doc = read_codex_config()?;
    let installed = plugin_entry(&doc).is_some();
    // The table `marketplace_moved_from` reads the source from.
    let listed = doc.get(MARKETPLACES).and_then(|m| m.get(NAME)).is_some();
    if !installed && !listed {
        host_plugin::remove_if_ours(root, &marketplace_manifest(root), host_plugin::names_us)?;
        return Ok(Unregistered::Absent);
    }
    if CLI.locate().is_none() {
        // A marketplace Codex still lists must keep its manifest.
        tracing::warn!("codex not on PATH; leaving the pixtuoid plugin registered");
        return Ok(Unregistered::Unreachable);
    }
    if installed {
        CLI.run(&[
            "plugin".as_ref(),
            PLUGIN_REMOVE.as_ref(),
            host_plugin::plugin_id().as_ref(),
        ])?;
    }
    if listed {
        remove_marketplace()?;
    }
    host_plugin::remove_if_ours(root, &marketplace_manifest(root), host_plugin::names_us)?;
    Ok(Unregistered::Removed)
}

fn is_registered() -> Result<bool> {
    let doc = read_codex_config()?;
    Ok(plugin_entry(&doc)
        .and_then(|e| e.get(ENABLED))
        .and_then(toml::Value::as_bool)
        == Some(true))
}

/// Where `codex plugin add` records the plugin and whether it is enabled
/// (`codex-rs/config/src/plugin_edit.rs` @ rust-v0.162.1).
fn plugin_entry(doc: &toml::Value) -> Option<&toml::Value> {
    doc.get(PLUGINS)?.get(host_plugin::plugin_id().as_str())
}

/// An empty table for a missing or empty file.
fn read_codex_config() -> Result<toml::Value> {
    let path = config_toml()?;
    let content = io::read_config(&path)?;
    toml::from_str(&content).with_context(|| format!("parsing {}", path.display()))
}

/// Strips what a pixtuoid from before the plugin merged into `config.toml`.
pub(crate) fn merge_uninstall(content: &str) -> Result<MergeOutcome> {
    crate::install::merge::toml_merge_outcome(content, toml_merge_uninstall)
}

fn handler_is_managed(h: &toml::Value) -> bool {
    // Sentinel-only, no basename fallback: no released version ever wrote a
    // sentinel-less entry, so a basename match could only ever hit a USER
    // hand-written entry pointing at the shim, which uninstall must not touch.
    h.get(SENTINEL_KEY).and_then(|v| v.as_bool()) == Some(true)
}

fn prune_managed_handlers(group: &mut toml::Value) {
    if let Some(hooks) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
        hooks.retain(|h| !handler_is_managed(h));
    }
}

fn group_has_no_hooks(group: &toml::Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|h| h.is_empty())
}

fn toml_merge_uninstall(mut root: toml::Table) -> toml::Table {
    let Some(toml::Value::Table(hooks)) = root.get_mut("hooks") else {
        return root;
    };
    for (_ev, list) in hooks.iter_mut() {
        if let Some(arr) = list.as_array_mut() {
            for group in arr.iter_mut() {
                prune_managed_handlers(group);
            }
            arr.retain(|group| !group_has_no_hooks(group));
        }
    }
    let empty: Vec<String> = hooks
        .iter()
        .filter_map(|(k, v)| match v.as_array() {
            Some(a) if a.is_empty() => Some(k.clone()),
            _ => None,
        })
        .collect();
    for k in empty {
        hooks.remove(&k);
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> toml::Value {
        toml::from_str(s).unwrap()
    }

    /// One event as a pixtuoid from before the plugin merged it into config.toml.
    const LEGACY: &str = r#"
[[hooks.PreToolUse]]
[[hooks.PreToolUse.hooks]]
type = "command"
command = "PIXTUOID_SOURCE=codex '/x/pixtuoid-hook'"
timeout = 5
statusMessage = "pixtuoid visualizer"
_pixtuoid = true
"#;

    #[test]
    fn the_hooks_file_sits_in_pixtuoids_own_codex_marketplace() {
        let base = std::env::temp_dir().join("pixtuoid-xdg");
        temp_env::with_var("XDG_CONFIG_HOME", Some(&base), || {
            assert_eq!(
                default_config_path().unwrap(),
                base.join("pixtuoid/codex-plugin/pixtuoid/hooks/hooks.json")
            );
        });
    }

    #[test]
    fn the_legacy_config_honors_codex_home() {
        let tmp = tempfile::tempdir().unwrap();
        temp_env::with_var("CODEX_HOME", Some(tmp.path()), || {
            assert_eq!(config_toml().unwrap(), tmp.path().join("config.toml"));
        });
        // A non-existent dir falls back, where upstream errors; `resolve_codex_home` says why.
        let missing = tmp.path().join("missing");
        temp_env::with_var("CODEX_HOME", Some(&missing), || {
            assert!(
                config_toml()
                    .unwrap()
                    .ends_with(PathBuf::from(".codex").join("config.toml"))
            );
        });
    }

    #[test]
    fn every_event_runs_the_shim_with_no_matcher_and_reads_it_back() {
        let cmd = "PIXTUOID_SOURCE=codex '/opt/bin/pixtuoid-hook'";
        let doc: serde_json::Value = serde_json::from_str(&render_hooks(cmd).unwrap()).unwrap();
        let hooks = doc["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), CODEX_EVENTS.len());
        for ev in CODEX_EVENTS {
            let group = &hooks[*ev][0];
            assert!(group.get("matcher").is_none(), "{ev}: {group}");
            assert_eq!(group["hooks"][0]["command"], json!(cmd));
            assert!(group["hooks"][0].get(SENTINEL_KEY).is_none());
        }
        assert_eq!(
            doc.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["hooks"],
            "Codex's `HooksFile` is `deny_unknown_fields` (`codex-rs/config/src/hook_config.rs` @ rust-v0.162.1)"
        );
        let v = verify_schema(&render_hooks(cmd).unwrap());
        assert!(v.issues.is_empty(), "{:?}", v.issues);
        #[cfg(unix)]
        assert_eq!(
            v.shim,
            ShimRef::Absolute(PathBuf::from("/opt/bin/pixtuoid-hook"))
        );
    }

    #[test]
    fn uninstall_no_pixtuoid_hooks_reports_unchanged() {
        let cfg = "model = \"o1\"\n\n[[hooks.PreToolUse]]\nmatcher = \"*\"\n\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"/usr/bin/mytool\"\n";
        let out = merge_uninstall(cfg).unwrap();
        assert!(!out.changed, "no managed entries → semantic no-op");
    }

    #[test]
    fn uninstall_keeps_user_handler_in_mixed_group() {
        let mut v = parse(LEGACY);
        let group = &mut v["hooks"]["PreToolUse"].as_array_mut().unwrap()[0];
        group["hooks"]
            .as_array_mut()
            .unwrap()
            .push(toml::Value::Table({
                let mut t = toml::value::Table::new();
                t.insert("type".into(), "command".into());
                t.insert("command".into(), "/usr/bin/mytool".into());
                t
            }));
        let cleaned = merge_uninstall(&toml::to_string_pretty(&v).unwrap()).unwrap();
        assert!(cleaned.changed, "the managed handler was removed");
        let cv = parse(&cleaned.content);
        let arr = cv["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "group kept (user handler remains)");
        let hooks = arr[0]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0]["command"].as_str().unwrap(), "/usr/bin/mytool");
    }

    #[test]
    fn uninstall_removes_empty_groups_and_events() {
        let cleaned = merge_uninstall(LEGACY).unwrap();
        assert!(cleaned.changed);
        let v = parse(&cleaned.content);
        assert!(
            v.get("hooks").is_none(),
            "all managed → hooks table dropped: {}",
            cleaned.content
        );
    }

    #[test]
    fn uninstall_leaves_a_hand_written_shim_entry_alone() {
        let cfg = r#"
[[hooks.PreToolUse]]
matcher = "*"
[[hooks.PreToolUse.hooks]]
type = "command"
command = "/hand/written/pixtuoid-hook"
"#;
        let cleaned = merge_uninstall(cfg).unwrap();
        let v = parse(&cleaned.content);
        assert!(
            v.get("hooks").is_some(),
            "hand-written entry survives uninstall: {}",
            cleaned.content
        );
        assert!(!cleaned.changed, "nothing managed to remove");
    }

    #[test]
    #[cfg(windows)]
    fn hook_command_emits_bare_exec_form_with_source_flag_on_windows() {
        let cmd = hook_command(std::path::Path::new(r"C:\tools\pixtuoid-hook.exe"), false).unwrap();
        assert_eq!(cmd, r"C:\tools\pixtuoid-hook.exe --source codex");
    }

    // These paths don't exist on the runner, so GetShortPathNameW fails and the
    // 8.3 substitution falls through to the reject arm (the 8.3-success path is
    // covered in hook_cmd/windows.rs).
    #[test]
    #[cfg(windows)]
    fn hook_command_rejects_cmd_unsafe_path_on_windows() {
        assert!(
            hook_command(
                std::path::Path::new(r"C:\Program Files\pixtuoid-hook.exe"),
                false
            )
            .is_err()
        );
        let err = hook_command(
            std::path::Path::new(r"C:\Users\a&b\pixtuoid-hook.exe"),
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("cmd.exe") && err.contains("ordinary characters"),
            "must explain the cmd-unsafe path + workaround: {err}"
        );
        for bad in [
            r"C:\p|x\h.exe",
            r"C:\p>x\h.exe",
            r"C:\p(x)\h.exe",
            r"C:\p%x\h.exe",
        ] {
            assert!(
                hook_command(std::path::Path::new(bad), false).is_err(),
                "must reject cmd-unsafe path {bad}"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn hook_command_errors_on_non_utf8_path() {
        use std::os::unix::ffi::OsStrExt;
        let bad = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/x/\xff/pixtuoid-hook"));
        assert!(hook_command(bad, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn hook_command_prefixes_source_for_valid_path() {
        let cmd = hook_command(std::path::Path::new("/opt/bin/pixtuoid-hook"), false).unwrap();
        assert_eq!(cmd, "PIXTUOID_SOURCE=codex '/opt/bin/pixtuoid-hook'");
    }

    #[cfg(unix)]
    #[test]
    fn hook_command_quotes_path_with_spaces() {
        let cmd = hook_command(
            std::path::Path::new("/Users/Jane Doe/bin/pixtuoid-hook"),
            false,
        )
        .unwrap();
        assert_eq!(
            cmd,
            "PIXTUOID_SOURCE=codex '/Users/Jane Doe/bin/pixtuoid-hook'"
        );
    }

    #[test]
    fn uninstall_doc_without_hooks_returns_unchanged() {
        let out = merge_uninstall("model = \"o1\"\n").unwrap();
        assert!(!out.changed, "no [hooks] → nothing to remove");
    }

    #[test]
    fn every_registered_codex_event_decodes() {
        use pixtuoid_core::source::decoder::decode_hook_payload;
        for ev in CODEX_EVENTS {
            // A complete-enough payload: `agent_id` satisfies SubagentStart/Stop;
            // the rest is ignored by events that don't need it.
            let payload = serde_json::json!({
                "hook_event_name": ev,
                "session_id": "sess",
                "agent_id": "child",
                "cwd": "/repo",
                "_pixtuoid_source": "codex",
            });
            assert!(
                decode_hook_payload(payload).is_ok(),
                "registered Codex hook {ev:?} has no decoder arm — it would bail \
                 as unsupported. Add an arm in pixtuoid-core source/decoder.rs."
            );
        }
    }

    #[test]
    fn codex_events_pins_the_exact_registered_set() {
        crate::install::assert_event_roster(
            "CODEX_EVENTS",
            CODEX_EVENTS,
            &[
                "SessionStart",
                "PreToolUse",
                "PostToolUse",
                "UserPromptSubmit",
                "SubagentStart",
                "SubagentStop",
                "Stop",
                "PermissionRequest",
                "SessionEnd",
            ],
        );
    }
}
