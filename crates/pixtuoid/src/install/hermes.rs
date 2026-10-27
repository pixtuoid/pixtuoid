//! Hermes Agent hook install target — a Python PLUGIN its gateway loads.
//!
//! Hermes runs every turn in its gateway, so pixtuoid drops a plugin at
//! `<hermes-home>/plugins/pixtuoid/` (the `hermes_plugin.py` template with the
//! shim's absolute path baked in, beside a `plugin.yaml`) and enables it through
//! `hermes plugins enable`, the documented path for a user plugin
//! (`website/docs/user-guide/features/plugins.md:97` at v0.22.5), which also loads
//! it into a running gateway.
//!
//! An older pixtuoid merged shell hooks into `config.yaml` instead, and their
//! payload's `cwd` is the gateway's own dir. Connect and disconnect strip them
//! ([`LegacyConfig`]); that YAML rewrite keeps the user's other keys but drops
//! comments (a saphyr limitation).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use saphyr::{LoadableYamlNode, MappingOwned, ScalarOwned, Yaml, YamlEmitter, YamlOwned};

use crate::install::SENTINEL_KEY;
use crate::install::host_plugin::{self, NAME};
use crate::install::io;
use crate::install::merge::{self, SentinelPlugin};
use crate::install::target::{HostRegistration, LegacyConfig, MergeOutcome, Pickup, Unregistered};
use crate::install::verify::{SchemaParse, ShimRef};

pub(crate) const HOST: HostRegistration = HostRegistration {
    cli: CLI,
    register,
    unregister,
    is_registered,
    pickup: Some(Pickup::Reload(reload)),
    legacy: Some(LegacyConfig {
        path: config_path,
        strip: strip_shell_hooks,
    }),
};

const CLI: io::HostCli = io::HostCli { command: "hermes" };

/// Marks the plugin as ours — absent from the removed-stub, so an uninstall of
/// a foreign or removed file is a clean no-op.
const SENTINEL: &str = "@pixtuoid-hermes-plugin";

const HOOK_PLACEHOLDER: &str = "\"{{HOOK_PATH_JSON}}\"";

/// The template's shim declaration, as [`crate::install::verify::baked_hook_path_after`]
/// finds it.
const HOOK_DECL: &str = "HOOK_PATH =";

const PLUGIN_TEMPLATE: &str = include_str!("hermes_plugin.py");

/// A directory plugin is a manifest and an `__init__.py` exposing `register(ctx)`
/// (`hermes_cli/plugins.py:6-7`).
const MANIFEST: &str = "plugin.yaml";
const PLUGIN_FILE: &str = "__init__.py";

/// The `hermes plugins` subcommands, and the `config.yaml` list `enable` keeps.
const PLUGINS: &str = "plugins";
const ENABLE: &str = "enable";
const DISABLE: &str = "disable";
const REMOVE: &str = "remove";
const ENABLED: &str = "enabled";

/// A plugin that registers nothing and carries no sentinel: what a disconnect
/// leaves should hermes stay unreachable, and a re-uninstall no-ops on it.
pub(crate) const REMOVED_STUB: &str = "# pixtuoid hermes plugin removed by disconnecting Hermes Agent in pixtuoid's Sources panel (press s).\n\n\ndef register(ctx):\n    pass\n";

const PLUGIN: SentinelPlugin = SentinelPlugin {
    sentinel: SENTINEL,
    placeholder: HOOK_PLACEHOLDER,
    removed_stub: REMOVED_STUB,
    what: "the Hermes plugin",
    cli: "Hermes Agent",
};

/// The events the plugin forwards: its `FORWARD` tuple.
#[cfg(test)]
pub(crate) fn plugin_events() -> Vec<&'static str> {
    template_tuple("FORWARD")
}

/// The items of the template's `<name> = ( … )` tuple.
#[cfg(test)]
fn template_tuple(name: &str) -> Vec<&'static str> {
    crate::install::template_list(PLUGIN_TEMPLATE, &format!("\n{name} = ("), ")")
}

fn home() -> Result<PathBuf> {
    pixtuoid_core::source::hermes::hermes_home()
        .ok_or_else(|| io::home_unset("the Hermes home", "HERMES_HOME/HOME"))
}

/// Where hermes discovers a user plugin (`hermes_cli/plugins.py:3-4`).
pub(crate) fn default_config_path() -> Result<PathBuf> {
    Ok(home()?.join(PLUGINS).join(NAME).join(PLUGIN_FILE))
}

/// The `config.yaml` hermes keeps its plugin lists in, and an older pixtuoid its
/// shell hooks.
fn config_path() -> Result<PathBuf> {
    Ok(home()?.join("config.yaml"))
}

/// Probes the home DIR, not config.yaml: Hermes creates the home on first run but may
/// not write config.yaml until `hermes setup`.
pub(crate) fn detect_installed() -> bool {
    pixtuoid_core::source::hermes::hermes_home().is_some_and(|d| d.exists())
}

/// The plugin spawns the shim by embedded path, so `_explicit` changes nothing.
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    merge::hook_path_str(resolved).map(str::to_string)
}

pub(crate) fn render_plugin(hook_path: &str) -> Result<String> {
    merge::bake_hook_path(PLUGIN_TEMPLATE, HOOK_PLACEHOLDER, hook_path, "hermes")
}

pub(crate) fn installed_shim(content: &str) -> ShimRef {
    crate::install::verify::baked_hook_path_after(content, HOOK_DECL)
        .map_or(ShimRef::Unknown, ShimRef::Absolute)
}

pub(crate) fn uninstall(content: &str) -> Result<MergeOutcome> {
    Ok(PLUGIN.uninstall(content))
}

pub(crate) fn verify_schema(content: &str) -> SchemaParse {
    crate::install::verify::sentinel_plugin_schema(&PLUGIN, content, installed_shim(content))
}

/// The plugin dir that holds `config`.
fn plugin_dir(config: &Path) -> Result<&Path> {
    config
        .parent()
        .with_context(|| format!("{} has no plugin dir", config.display()))
}

/// No `version`: none is required, and the manifest is rewritten on every connect.
fn manifest() -> String {
    format!(
        "name: {NAME}\ndescription: Sends Hermes Agent's session events to the pixtuoid office\n"
    )
}

fn register(config: &Path) -> Result<()> {
    let dir = plugin_dir(config)?;
    let manifest_path = dir.join(MANIFEST);
    let fresh = !manifest_path.exists();
    let enabled = is_registered()?;
    io::write_config_atomic(&manifest_path, &manifest())?;
    let registered = if enabled {
        reload()
    } else {
        CLI.run(&[PLUGINS.as_ref(), ENABLE.as_ref(), NAME.as_ref()])
            .map(drop)
    };
    if let Err(e) = registered {
        // Hermes lists any user dir that holds a manifest, code or none
        // (`hermes_cli/plugins_cmd.py:869-881` at v0.22.5).
        if fresh && let Err(undo) = host_plugin::remove_if_ours(dir, config, is_ours) {
            return Err(e.context(format!("its manifest stays in place: {undo:#}")));
        }
        return Err(e);
    }
    Ok(())
}

/// `enable` reloads the gateway only for a selection it changed
/// (`hermes_cli/plugins_cmd.py:726-732` at v0.22.5), so the plugin is disabled,
/// which unloads it, then enabled, which loads the files on disk
/// (`hermes_cli/plugins_activation.py:118-121`).
fn reload() -> Result<()> {
    CLI.run(&[PLUGINS.as_ref(), DISABLE.as_ref(), NAME.as_ref()])?;
    CLI.run(&[PLUGINS.as_ref(), ENABLE.as_ref(), NAME.as_ref()])?;
    Ok(())
}

/// Disable first: it unloads the plugin from a running gateway, which `remove`
/// leaves firing (`hermes_cli/plugins_cmd.py:753-766`, `plugins_cmd_remove.py:104-117`
/// at v0.22.5). `remove` then deletes the dir and the config's every trace of it.
fn unregister(config: &Path) -> Result<Unregistered> {
    let dir = plugin_dir(config)?;
    if !dir.join(MANIFEST).exists() {
        host_plugin::remove_if_ours(dir, config, is_ours)?;
        return Ok(Unregistered::Absent);
    }
    if CLI.locate().is_none() {
        tracing::warn!("hermes not on PATH; leaving the pixtuoid plugin registered");
        return Ok(Unregistered::Unreachable);
    }
    CLI.run(&[PLUGINS.as_ref(), DISABLE.as_ref(), NAME.as_ref()])?;
    CLI.run(&[PLUGINS.as_ref(), REMOVE.as_ref(), NAME.as_ref()])?;
    Ok(Unregistered::Removed)
}

/// Our plugin, or the stub its disconnect leaves.
fn is_ours(plugin: &str) -> bool {
    plugin.contains(SENTINEL) || plugin == REMOVED_STUB
}

/// Read-only, so a status check starts no CLI.
fn is_registered() -> Result<bool> {
    let root = parse_root_mapping(&io::read_config(&config_path()?)?)?;
    let listed = root
        .get(&ystr(PLUGINS))
        .and_then(|p| match p {
            YamlOwned::Mapping(m) => m.get(&ystr(ENABLED)),
            _ => None,
        })
        .is_some_and(
            |enabled| matches!(enabled, YamlOwned::Sequence(names) if names.contains(&ystr(NAME))),
        );
    Ok(listed)
}

fn ystr(s: &str) -> YamlOwned {
    YamlOwned::Value(ScalarOwned::String(s.to_string()))
}

fn ybool(b: bool) -> YamlOwned {
    YamlOwned::Value(ScalarOwned::Boolean(b))
}

fn parse_root_mapping(content: &str) -> Result<MappingOwned> {
    if content.trim().is_empty() {
        return Ok(MappingOwned::new());
    }
    let docs = YamlOwned::load_from_str(content)
        .map_err(|e| anyhow!("config.yaml no longer parses as YAML: {e}"))?;
    match docs.into_iter().next() {
        None => Ok(MappingOwned::new()),
        Some(YamlOwned::Mapping(m)) => Ok(m),
        Some(_) => bail!("config.yaml is valid YAML but its root is not a mapping"),
    }
}

/// Strips saphyr's leading `---` document-start marker for a cleaner diff; Hermes parses
/// it either way, and a hand-written config.yaml has none.
fn emit(root: &MappingOwned) -> Result<String> {
    let doc = YamlOwned::Mapping(root.clone());
    let borrowed = Yaml::from(&doc);
    let mut out = String::new();
    YamlEmitter::new(&mut out)
        .dump(&borrowed)
        .map_err(|e| anyhow!("emitting config.yaml: {e}"))?;
    Ok(out.strip_prefix("---\n").map(str::to_string).unwrap_or(out))
}

fn is_managed(entry: &YamlOwned) -> bool {
    matches!(entry, YamlOwned::Mapping(m) if m.get(&ystr(SENTINEL_KEY)) == Some(&ybool(true)))
}

/// Remove every `hooks:` entry an older pixtuoid merged (its `_pixtuoid: true`
/// sentinel), dropping an event, and then `hooks:` itself, it emptied.
pub(crate) fn strip_shell_hooks(content: &str) -> Result<MergeOutcome> {
    let mut root = parse_root_mapping(content).context("refusing to overwrite config.yaml")?;
    let hooks_key = ystr("hooks");
    let Some(YamlOwned::Mapping(hooks)) = root.get_mut(&hooks_key) else {
        return Ok(MergeOutcome {
            content: emit(&root)?,
            changed: false,
        });
    };

    let mut changed = false;
    let mut emptied_events: Vec<YamlOwned> = Vec::new();
    for (k, v) in hooks.iter_mut() {
        if let YamlOwned::Sequence(seq) = v {
            let before = seq.len();
            seq.retain(|e| !is_managed(e));
            if seq.len() != before {
                changed = true;
                if seq.is_empty() {
                    emptied_events.push(k.clone());
                }
            }
        }
    }
    for k in emptied_events {
        hooks.remove(&k);
    }
    // Gated on `changed` so an already-empty user `hooks:` is never touched.
    if changed && matches!(root.get(&hooks_key), Some(YamlOwned::Mapping(m)) if m.is_empty()) {
        root.remove(&hooks_key);
    }

    Ok(MergeOutcome {
        content: emit(&root)?,
        changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What an older pixtuoid merged, beside a user's own hook and key.
    const LEGACY: &str = "model: gpt\nhooks:\n  pre_tool_call:\n    - command: my-guard.sh\n    - command: \"'/opt/pixtuoid-hook' --source hermes\"\n      timeout: 5\n      _pixtuoid: true\n  on_session_finalize:\n    - command: \"'/opt/pixtuoid-hook' --source hermes\"\n      timeout: 5\n      _pixtuoid: true\n";

    #[test]
    fn the_legacy_strip_removes_only_managed_hooks_and_keeps_the_users() {
        let out = strip_shell_hooks(LEGACY).unwrap();
        assert!(out.changed);
        let root = parse_root_mapping(&out.content).unwrap();
        assert_eq!(root.get(&ystr("model")), Some(&ystr("gpt")));
        let Some(YamlOwned::Mapping(hooks)) = root.get(&ystr("hooks")) else {
            panic!("the user's hook went: {}", out.content);
        };
        let Some(YamlOwned::Sequence(seq)) = hooks.get(&ystr("pre_tool_call")) else {
            panic!("{}", out.content);
        };
        assert_eq!(seq.len(), 1, "{}", out.content);
        assert!(
            hooks.get(&ystr("on_session_finalize")).is_none(),
            "an emptied event is dropped: {}",
            out.content
        );
        assert!(!strip_shell_hooks(&out.content).unwrap().changed);
    }

    #[test]
    fn the_legacy_strip_drops_a_hooks_mapping_it_emptied_but_never_a_users_empty_one() {
        let only_ours =
            "model: gpt\nhooks:\n  on_session_start:\n    - command: x\n      _pixtuoid: true\n";
        let out = strip_shell_hooks(only_ours).unwrap();
        let root = parse_root_mapping(&out.content).unwrap();
        assert!(root.get(&ystr("hooks")).is_none(), "{}", out.content);
        assert_eq!(root.get(&ystr("model")), Some(&ystr("gpt")));
        assert!(!strip_shell_hooks("hooks: {}\n").unwrap().changed);
        assert!(!strip_shell_hooks("").unwrap().changed);
        let refused = strip_shell_hooks("- 1\n- 2\n").unwrap_err();
        assert!(format!("{refused:#}").contains("refusing to overwrite"));
    }

    /// The plugin forwards EXACTLY what the decoder reads: two lists in two
    /// languages no compiler sees across, bound through core's `DECODED_EVENTS`.
    /// Forward one the decoder bails on and every event of it is a decode error;
    /// miss one it reads and that signal never arrives.
    #[test]
    fn the_plugin_forwards_exactly_what_the_decoder_reads() {
        let forwarded: std::collections::BTreeSet<&str> = plugin_events().into_iter().collect();
        let decoded: std::collections::BTreeSet<&str> =
            pixtuoid_core::source::hermes::DECODED_EVENTS
                .iter()
                .copied()
                .collect();
        assert_eq!(forwarded, decoded);
    }

    /// The plugin copies kwargs by name and the decoder's own tests build their
    /// payloads inline, so the two field lists are pinned here. `cwd` is the one
    /// field hermes's kwargs never carry: the plugin asks the session for it.
    #[test]
    fn the_plugin_copies_exactly_the_fields_the_decoder_reads() {
        use pixtuoid_core::source::hermes::{DECODED_FIELDS, DETAIL_KEYS};
        let mut copied = template_tuple("FIELDS");
        copied.push("cwd");
        copied.sort_unstable();
        let mut decoded = DECODED_FIELDS.to_vec();
        decoded.sort_unstable();
        assert_eq!(copied, decoded);
        assert_eq!(template_tuple("ARGS"), DETAIL_KEYS);
        assert_eq!(
            template_tuple("OUTSIDE_A_TURN"),
            pixtuoid_core::source::hermes::OUTSIDE_A_TURN,
            "an event outside a turn has no cwd to send, and no arm reads one"
        );
    }

    #[test]
    fn the_plugin_runs_the_shim_with_the_flag_its_writers_share() {
        let flag = crate::install::hook_cmd::SOURCE_FLAG.trim();
        let source = pixtuoid_core::source::hermes::SOURCE_NAME;
        assert!(PLUGIN_TEMPLATE.contains(&format!(r#"[HOOK_PATH, "{flag}", "{source}"]"#)));
    }

    #[test]
    fn the_rendered_plugin_bakes_the_shim_and_verifies_sound() {
        let installed = render_plugin("/Users/Jane Doe/bin/pixtuoid-hook").unwrap();
        assert!(installed.contains(r#"HOOK_PATH = "/Users/Jane Doe/bin/pixtuoid-hook""#));
        let sound = verify_schema(&installed);
        assert!(sound.issues.is_empty(), "{:?}", sound.issues);
        assert_eq!(
            sound.shim,
            ShimRef::Absolute(PathBuf::from("/Users/Jane Doe/bin/pixtuoid-hook")),
            "the baked HOOK_PATH must be reported, or the shim-on-disk check is skipped"
        );
    }

    #[test]
    fn verify_reports_every_way_the_plugin_can_be_dead() {
        let unbaked = PLUGIN_TEMPLATE.replace(HOOK_PLACEHOLDER, "None");
        for (content, why) in [
            ("def register(ctx):\n    pass\n", "reconnect Hermes Agent"),
            (PLUGIN_TEMPLATE, "never substituted"),
            (unbaked.as_str(), "could not read HOOK_PATH"),
        ] {
            let v = verify_schema(content);
            assert!(
                v.issues.iter().any(|i| i.contains(why)),
                "{why}: {:?}",
                v.issues
            );
            assert_eq!(v.shim, ShimRef::Unknown);
        }
    }

    #[test]
    fn uninstall_leaves_a_stub_that_is_no_longer_ours() {
        let installed = render_plugin("/opt/pixtuoid-hook").unwrap();
        let out = uninstall(&installed).unwrap();
        assert!(out.changed);
        assert_eq!(out.content, REMOVED_STUB);
        assert!(!uninstall(&out.content).unwrap().changed);
        assert!(!uninstall("").unwrap().changed);
        assert!(!uninstall("def register(ctx):\n    pass\n").unwrap().changed);
    }

    #[cfg(unix)]
    #[test]
    fn hook_command_is_the_absolute_path() {
        let cmd = hook_command(Path::new("/Users/Jane Doe/bin/pixtuoid-hook"), false).unwrap();
        assert_eq!(cmd, "/Users/Jane Doe/bin/pixtuoid-hook");
    }

    #[test]
    #[cfg(unix)]
    fn hook_command_errors_on_non_utf8_path() {
        use std::os::unix::ffi::OsStrExt;
        let bad = Path::new(std::ffi::OsStr::from_bytes(b"/x/\xff/pixtuoid-hook"));
        assert!(hook_command(bad, false).is_err());
    }

    #[test]
    fn the_plugins_forward_set_is_pinned() {
        crate::install::assert_event_roster(
            "the plugin's FORWARD",
            &plugin_events(),
            &[
                "on_session_start",
                "pre_tool_call",
                "pre_approval_request",
                "on_session_finalize",
                "post_tool_call",
                "on_session_end",
            ],
        );
    }

    #[test]
    fn is_registered_reads_hermes_own_enabled_list() {
        let home = tempfile::TempDir::new().unwrap();
        temp_env::with_var("HERMES_HOME", Some(home.path()), || {
            let config = home.path().join("config.yaml");
            assert!(!is_registered().unwrap(), "no config.yaml");
            for (content, want) in [
                ("plugins:\n  enabled:\n  - pixtuoid\n", true),
                (
                    "plugins:\n  enabled: []\n  disabled:\n  - pixtuoid\n",
                    false,
                ),
                ("plugins:\n  enabled:\n  - other\n", false),
            ] {
                std::fs::write(&config, content).unwrap();
                assert_eq!(is_registered().unwrap(), want, "{content}");
            }
            std::fs::write(&config, "- pixtuoid\n").unwrap();
            let unread = format!("{:#}", is_registered().unwrap_err());
            assert!(
                !unread.contains("refusing"),
                "a read declines no write: {unread}"
            );
        });
    }
}
