//! Antigravity CLI hook install target — a plugin agy discovers in
//! `~/.gemini/config/plugins/pixtuoid/` (antigravity.google/docs/plugins.md,
//! "Manual plugin installation"), so pixtuoid writes only its own files. `agy plugin
//! install` would only copy a dir to that same place.
//!
//! An older pixtuoid merged its hooks into the shared `~/.gemini/config/hooks.json`;
//! connect and disconnect strip them ([`LegacyConfig`]).
//!
//! agy's payload names no event, so each command bakes [`HOOK_EVENT_FLAG`] and the
//! event for the shim to stamp.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use crate::install::hook_cmd::HOOK_EVENT_FLAG;
use crate::install::host_plugin::{self, NAME};
use crate::install::io;
use crate::install::merge;
use crate::install::target::{HostRegistration, LegacyConfig, MergeOutcome, Unregistered};
use crate::install::verify::{SchemaParse, ShimRef, assemble, shell_shim_ref};
use pixtuoid_core::source::antigravity::{CLI_APP_DATA_DIR, SOURCE_NAME, gemini_dir};

pub(crate) const HOST: HostRegistration = HostRegistration {
    cli: CLI,
    register,
    unregister,
    is_registered,
    pickup: None,
    legacy: Some(LegacyConfig {
        path: legacy_hooks_path,
        strip: strip_legacy_hooks,
    }),
};

const CLI: io::HostCli = io::HostCli { command: "agy" };

const HOOKS_FILE: &str = "hooks.json";
const MANIFEST: &str = "plugin.json";

/// The `agy plugin` subcommands.
const PLUGIN: &str = "plugin";
const ENABLE: &str = "enable";
const UNINSTALL: &str = "uninstall";

/// Events we register == events we decode (`source/antigravity.rs`). The bool
/// says whether the event's handlers sit in a `{matcher, hooks}` group, as a
/// tool event's must.
pub(crate) const AG_EVENTS: &[(&str, bool)] = &[
    ("SessionStart", false),
    ("PreToolUse", true),
    ("PostToolUse", true),
    ("Stop", false),
];

fn config_dir() -> Result<PathBuf> {
    if pixtuoid_core::platform::user_home_opt().is_none() {
        return Err(io::home_unset("the home directory", "HOME/USERPROFILE"));
    }
    Ok(gemini_dir().join("config"))
}

pub(crate) fn default_config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("plugins").join(NAME).join(HOOKS_FILE))
}

fn legacy_hooks_path() -> Result<PathBuf> {
    Ok(config_dir()?.join(HOOKS_FILE))
}

/// agy creates its app data dir on first run; `~/.gemini` alone is Gemini CLI's
/// too.
pub(crate) fn detect_installed() -> bool {
    gemini_dir().join(CLI_APP_DATA_DIR).exists()
}

/// The BASE command, which [`render_hooks`] extends per event. agy shells it
/// (`sh -c`, `cmd /c`), so the OS forms are
/// [`crate::install::hook_cmd::shell_hook_command`]'s.
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    let p = merge::hook_path_str(resolved)?;
    crate::install::hook_cmd::shell_hook_command(p, SOURCE_NAME)
}

pub(crate) fn render_hooks(base_cmd: &str) -> Result<String> {
    Ok(serde_json::to_string_pretty(
        &json!({ NAME: hook_set(base_cmd) }),
    )?)
}

pub(crate) fn installed_shim(content: &str) -> ShimRef {
    verify_schema(content).shim
}

/// What a disconnect leaves should agy stay unreachable: a plugin with no hooks.
pub(crate) fn uninstall(content: &str) -> Result<MergeOutcome> {
    let ours = serde_json::from_str::<Value>(content).is_ok_and(|doc| doc.get(NAME).is_some());
    Ok(MergeOutcome {
        content: "{}\n".to_string(),
        changed: ours,
    })
}

fn strip_legacy_hooks(content: &str) -> Result<MergeOutcome> {
    merge::flat_json_merge_outcome_uninstall(content, |mut doc| {
        if let Some(root) = doc.as_object_mut() {
            root.remove(NAME);
        }
        doc
    })
}

/// `enable` because agy keeps a user's earlier `disable`, and a connect overrides it.
fn register(config: &Path) -> Result<()> {
    let (plugin, _) = host_plugin::layout(config, &[HOOKS_FILE])?;
    let manifest = json!({
        "name": NAME,
        "description": "Sends Antigravity CLI's session events to the pixtuoid office",
    });
    host_plugin::write_manifests(&[(plugin.join(MANIFEST), manifest)])?;
    CLI.run(&[PLUGIN.as_ref(), ENABLE.as_ref(), NAME.as_ref()])
        .map(drop)
}

fn unregister(config: &Path) -> Result<Unregistered> {
    let (plugin, _) = host_plugin::layout(config, &[HOOKS_FILE])?;
    if !plugin.join(MANIFEST).exists() {
        return Ok(Unregistered::Absent);
    }
    if CLI.locate().is_none() {
        tracing::warn!("agy not on PATH; leaving the pixtuoid plugin registered");
        return Ok(Unregistered::Unreachable);
    }
    CLI.run(&[PLUGIN.as_ref(), UNINSTALL.as_ref(), NAME.as_ref()])?;
    Ok(Unregistered::Removed)
}

/// agy enables a plugin it finds until told otherwise. Read-only, so a status check
/// starts no CLI.
fn is_registered() -> Result<bool> {
    let path = config_dir()?.join("config.json");
    let config = merge::parse_json_or_empty(&io::read_config(&path)?)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(config["plugins"][NAME]["enabled"] != json!(false))
}

fn hook_set(base_cmd: &str) -> Value {
    let set: Map<String, Value> = AG_EVENTS
        .iter()
        .map(|&(event, grouped)| {
            let handler = json!({
                "type": "command",
                "command": format!("{base_cmd}{HOOK_EVENT_FLAG}{event}"),
            });
            let handlers = if grouped {
                json!([{ "matcher": "*", "hooks": [handler] }])
            } else {
                json!([handler])
            };
            (event.to_owned(), handlers)
        })
        .collect();
    Value::Object(set)
}

/// The command of `event`'s first handler in pixtuoid's set, in the shape
/// [`hook_set`] writes it.
fn installed_command<'a>(set: &'a Value, event: &str, grouped: bool) -> Option<&'a str> {
    let first = set.get(event)?.get(0)?;
    let handler = if grouped {
        first.get("hooks")?.get(0)?
    } else {
        first
    };
    handler.get("command")?.as_str()
}

pub(crate) fn verify_schema(content: &str) -> SchemaParse {
    let Ok(doc) = serde_json::from_str::<Value>(content) else {
        return SchemaParse::broken("hooks config no longer parses as JSON");
    };
    let Some(set) = doc.get(NAME) else {
        return SchemaParse::broken(format!(
            "no `{NAME}` hook in hooks.json (hand-edited, or never installed)"
        ));
    };
    let mut missing = Vec::new();
    let mut mislabelled = Vec::new();
    let mut shim = ShimRef::Unknown;
    for &(event, grouped) in AG_EVENTS {
        let Some(cmd) = installed_command(set, event, grouped) else {
            missing.push(event);
            continue;
        };
        let own = shell_shim_ref(cmd);
        let other_shim = if shim == ShimRef::Unknown {
            shim = own;
            false
        } else {
            own != shim
        };
        // The shim stamps whatever event the command names.
        if other_shim || !cmd.ends_with(&format!("{HOOK_EVENT_FLAG}{event}")) {
            mislabelled.push(event);
        }
    }
    let mut extra = Vec::new();
    if !mislabelled.is_empty() {
        extra.push(format!(
            "the `{NAME}` hook's command for {} names another event or shim",
            mislabelled.join(", ")
        ));
    }
    // agy skips a hook switched off by `"enabled": false` (antigravity.google/docs/hooks.md,
    // "Hook definition fields").
    if set.get("enabled") == Some(&Value::Bool(false)) {
        extra.push(format!(
            "the `{NAME}` hook is disabled (`\"enabled\": false`)"
        ));
    }
    assemble(&missing, true, shim, extra)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "PIXTUOID_SOURCE=antigravity '/opt/pixtuoid-hook'";

    fn rendered() -> Value {
        serde_json::from_str(&render_hooks(CMD).expect("render")).expect("json")
    }

    #[test]
    fn the_plugin_holds_one_named_hook_per_agy_shape() {
        let doc = rendered();
        let set = &doc[NAME];
        for &(event, grouped) in AG_EVENTS {
            assert_eq!(
                installed_command(set, event, grouped),
                Some(format!("{CMD} --hook-event {event}").as_str()),
                "{event}"
            );
            if grouped {
                assert_eq!(set[event][0]["matcher"], "*", "{event} matches every tool");
            } else {
                assert!(set[event][0].get("hooks").is_none(), "{event} is flat");
            }
        }
    }

    #[test]
    fn the_legacy_strip_removes_only_pixtuoids_hook() {
        let user = r#"{"lint": {"Stop": []}}"#;
        let with = json!({ "lint": {"Stop": []}, NAME: hook_set(CMD) }).to_string();
        let out = strip_legacy_hooks(&with).expect("strip");
        assert!(out.changed);
        let doc: Value = serde_json::from_str(&out.content).expect("json");
        assert!(doc.get(NAME).is_none());
        assert_eq!(doc["lint"], json!({"Stop": []}));
        assert!(!strip_legacy_hooks(user).expect("absent").changed);
        assert!(!strip_legacy_hooks("").expect("empty").changed);
    }

    #[test]
    fn an_uninstall_is_ours_only_on_a_plugin_with_our_hook() {
        assert!(uninstall(&render_hooks(CMD).unwrap()).unwrap().changed);
        assert!(!uninstall("{}\n").unwrap().changed);
        assert!(!uninstall("not json").unwrap().changed);
    }

    #[test]
    fn a_rendered_plugin_verifies_and_names_its_shim() {
        let parse = verify_schema(&render_hooks(CMD).expect("render"));
        assert!(parse.issues.is_empty(), "{:?}", parse.issues);
        assert_eq!(
            parse.shim,
            ShimRef::Absolute(PathBuf::from("/opt/pixtuoid-hook"))
        );
    }

    #[test]
    fn verify_flags_a_missing_event_a_disabled_hook_and_no_hook() {
        let mut doc = rendered();
        doc[NAME]
            .as_object_mut()
            .expect("set")
            .remove("PostToolUse");
        let parse = verify_schema(&doc.to_string());
        assert!(
            parse.issues.iter().any(|i| i.contains("PostToolUse")),
            "{:?}",
            parse.issues
        );

        let mut doc = rendered();
        doc[NAME]["SessionStart"][0]["command"] = json!(format!("{CMD} --hook-event Stop"));
        let parse = verify_schema(&doc.to_string());
        assert!(
            parse.issues.iter().any(|i| i.contains("SessionStart")),
            "{:?}",
            parse.issues
        );

        let mut doc = rendered();
        doc[NAME]["Stop"][0]["command"] = json!(format!(
            "{} --hook-event Stop",
            CMD.replace("/opt/", "/old/")
        ));
        let parse = verify_schema(&doc.to_string());
        assert!(
            parse.issues.iter().any(|i| i.contains("Stop")),
            "{:?}",
            parse.issues
        );

        let mut doc = rendered();
        doc[NAME]["enabled"] = json!(false);
        let parse = verify_schema(&doc.to_string());
        assert!(
            parse.issues.iter().any(|i| i.contains("disabled")),
            "{:?}",
            parse.issues
        );

        assert!(!verify_schema(r#"{"lint": {}}"#).issues.is_empty());
        assert!(!verify_schema("not json").issues.is_empty());
    }

    #[test]
    fn every_registered_event_decodes() {
        use pixtuoid_core::source::decoder::decode_hook_payload;
        for &(event, _) in AG_EVENTS {
            let payload = json!({
                pixtuoid_core::source::decoder::EVENT_STAMP: event,
                "_pixtuoid_source": SOURCE_NAME,
                "conversationId": "c1",
                "workspacePaths": ["/repo"],
                "stepIdx": 2,
                "toolCall": { "name": "view_file", "args": {} },
            });
            assert!(
                decode_hook_payload(payload).is_ok_and(|evs| !evs.is_empty()),
                "registered agy hook {event:?} has no decoder arm in pixtuoid-core \
                 source/antigravity.rs"
            );
        }
    }

    #[test]
    fn ag_events_pins_the_exact_registered_set() {
        crate::install::assert_event_roster(
            "AG_EVENTS",
            &AG_EVENTS.iter().map(|&(name, _)| name).collect::<Vec<_>>(),
            &["SessionStart", "PreToolUse", "PostToolUse", "Stop"],
        );
    }
}
