//! Antigravity CLI hook install target — the GLOBAL `~/.gemini/config/hooks.json`
//! (antigravity.google/docs/hooks.md, "Managing hooks in Antigravity CLI").
//!
//! Its top-level keys are hook NAMES, so pixtuoid's whole set is the one
//! [`HOOK_NAME`] key: an install sets it, an uninstall removes it, and no other
//! hook is touched. agy's payload names no event, so each command bakes
//! [`HOOK_EVENT_FLAG`] and the event for the shim to stamp.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use crate::install::hook_cmd::HOOK_EVENT_FLAG;
use crate::install::io;
use crate::install::merge;
use crate::install::target::MergeOutcome;
use crate::install::verify::{SchemaParse, ShimRef, assemble, shell_shim_ref};
use pixtuoid_core::source::antigravity::{CLI_APP_DATA_DIR, SOURCE_NAME, gemini_dir};

/// The hook name pixtuoid's set lives under.
pub(crate) const HOOK_NAME: &str = "pixtuoid";

/// Events we register == events we decode (`source/antigravity.rs`). The bool
/// says whether the event's handlers sit in a `{matcher, hooks}` group, as a
/// tool event's must.
pub(crate) const AG_EVENTS: &[(&str, bool)] = &[
    ("SessionStart", false),
    ("PreToolUse", true),
    ("PostToolUse", true),
    ("Stop", false),
];

pub(crate) fn default_config_path() -> Result<PathBuf> {
    if pixtuoid_core::platform::user_home_opt().is_none() {
        return Err(io::home_unset("the home directory", "HOME/USERPROFILE"));
    }
    Ok(gemini_dir().join("config").join("hooks.json"))
}

/// agy creates its app data dir on first run; `~/.gemini` alone is Gemini CLI's
/// too.
pub(crate) fn detect_installed() -> bool {
    gemini_dir().join(CLI_APP_DATA_DIR).exists()
}

/// The BASE command, which `merge_install` extends per event. agy shells it
/// (`sh -c`, `cmd /c`), so the OS forms are
/// [`crate::install::hook_cmd::shell_hook_command`]'s.
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    let p = merge::hook_path_str(resolved)?;
    crate::install::hook_cmd::shell_hook_command(p, SOURCE_NAME)
}

pub(crate) fn merge_install(content: &str, base_cmd: &str) -> Result<MergeOutcome> {
    merge::flat_json_merge_outcome_install(content, "hooks.json", |doc| {
        let mut root = doc.as_object().cloned().unwrap_or_default();
        root.insert(HOOK_NAME.into(), hook_set(base_cmd));
        Value::Object(root)
    })
}

pub(crate) fn merge_uninstall(content: &str) -> Result<MergeOutcome> {
    merge::flat_json_merge_outcome_uninstall(content, |mut doc| {
        if let Some(root) = doc.as_object_mut() {
            root.remove(HOOK_NAME);
        }
        doc
    })
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
    let Some(set) = doc.get(HOOK_NAME) else {
        return SchemaParse::broken(format!(
            "no `{HOOK_NAME}` hook in hooks.json (hand-edited, or never installed)"
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
            "the `{HOOK_NAME}` hook's command for {} names another event or shim",
            mislabelled.join(", ")
        ));
    }
    // agy skips a hook switched off by `"enabled": false` (antigravity.google/docs/hooks.md,
    // "Hook definition fields").
    if set.get("enabled") == Some(&Value::Bool(false)) {
        extra.push(format!(
            "the `{HOOK_NAME}` hook is disabled (`\"enabled\": false`)"
        ));
    }
    assemble(&missing, true, shim, extra)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "PIXTUOID_SOURCE=antigravity '/opt/pixtuoid-hook'";

    fn installed(content: &str, cmd: &str) -> Value {
        let out = merge_install(content, cmd).expect("install");
        serde_json::from_str(&out.content).expect("json")
    }

    #[test]
    fn install_writes_one_named_hook_per_agy_shape() {
        let doc = installed("", CMD);
        let set = &doc[HOOK_NAME];
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
    fn install_keeps_the_users_hooks_and_is_idempotent() {
        let user = r#"{"lint": {"PostToolUse": [{"matcher": "run_command", "hooks": []}]}}"#;
        let first = merge_install(user, CMD).expect("install");
        assert!(first.changed);
        let doc: Value = serde_json::from_str(&first.content).expect("json");
        assert_eq!(
            doc["lint"],
            serde_json::from_str::<Value>(user).expect("json")["lint"]
        );
        assert!(!merge_install(&first.content, CMD).expect("again").changed);

        let moved = installed(
            &first.content,
            "PIXTUOID_SOURCE=antigravity '/new/pixtuoid-hook'",
        );
        assert_eq!(
            installed_command(&moved[HOOK_NAME], "Stop", false),
            Some("PIXTUOID_SOURCE=antigravity '/new/pixtuoid-hook' --hook-event Stop")
        );
    }

    #[test]
    fn uninstall_removes_only_pixtuoids_hook() {
        let user = r#"{"lint": {"Stop": []}}"#;
        let with = merge_install(user, CMD).expect("install").content;
        let out = merge_uninstall(&with).expect("uninstall");
        assert!(out.changed);
        let doc: Value = serde_json::from_str(&out.content).expect("json");
        assert!(doc.get(HOOK_NAME).is_none());
        assert_eq!(doc["lint"], json!({"Stop": []}));
        assert!(!merge_uninstall(user).expect("absent").changed);
        assert!(!merge_uninstall("").expect("empty").changed);
    }

    #[test]
    fn a_fresh_install_verifies_and_names_its_shim() {
        let parse = verify_schema(&merge_install("", CMD).expect("install").content);
        assert!(parse.issues.is_empty(), "{:?}", parse.issues);
        assert_eq!(
            parse.shim,
            ShimRef::Absolute(PathBuf::from("/opt/pixtuoid-hook"))
        );
    }

    #[test]
    fn verify_flags_a_missing_event_a_disabled_hook_and_no_hook() {
        let mut doc = installed("", CMD);
        doc[HOOK_NAME]
            .as_object_mut()
            .expect("set")
            .remove("PostToolUse");
        let parse = verify_schema(&doc.to_string());
        assert!(
            parse.issues.iter().any(|i| i.contains("PostToolUse")),
            "{:?}",
            parse.issues
        );

        let mut doc = installed("", CMD);
        doc[HOOK_NAME]["SessionStart"][0]["command"] = json!(format!("{CMD} --hook-event Stop"));
        let parse = verify_schema(&doc.to_string());
        assert!(
            parse.issues.iter().any(|i| i.contains("SessionStart")),
            "{:?}",
            parse.issues
        );

        let mut doc = installed("", CMD);
        doc[HOOK_NAME]["Stop"][0]["command"] = json!(format!(
            "{} --hook-event Stop",
            CMD.replace("/opt/", "/old/")
        ));
        let parse = verify_schema(&doc.to_string());
        assert!(
            parse.issues.iter().any(|i| i.contains("Stop")),
            "{:?}",
            parse.issues
        );

        let mut doc = installed("", CMD);
        doc[HOOK_NAME]["enabled"] = json!(false);
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
