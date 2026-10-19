//! Cursor CLI hook install target — the GLOBAL `<cursor-config-dir>/hooks.json`.
//! The `cursor-agent` CLI reads both that file and a project
//! `<repo>/.cursor/hooks.json`; we install user-global so it covers every project.
//!
//! Schema (`cursor.com/docs/hooks`) is a `version` + per-event arrays of FLAT
//! `{command}` entries — NOT Claude's nested `{matcher, hooks:[...]}` groups, which
//! reportedly do not fire in the Cursor CLI. `version` is required by Cursor (set to
//! 1 on install if absent, a user's value preserved); `_pixtuoid` is the
//! managed-entry sentinel, which Cursor's loader ignores as an unknown object field.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value, json};

use crate::install::SENTINEL_KEY;
use crate::install::io;
use crate::install::merge;
use crate::install::target::MergeOutcome;

/// Events we register == events we decode (`source/cursor.rs`), enforced by
/// `every_registered_cursor_event_decodes` below. `subagentStart`/`subagentStop`
/// are deliberately absent (they don't fire in the CLI). `postToolUseFailure` FIRES
/// instead of `postToolUse` on a failed tool, so it is registered too — else a
/// failed tool's ActivityStart never closes under `-p` (where `stop` doesn't fire).
pub(crate) const CURSOR_EVENTS: &[&str] = &[
    "sessionStart",
    "preToolUse",
    "postToolUse",
    "postToolUseFailure",
    "stop",
    "sessionEnd",
];

/// Mirrors Cursor's documented resolution: `CURSOR_CONFIG_DIR` (all platforms)
/// wins; else on **Linux/BSD** a set `XDG_CONFIG_HOME` gives
/// `$XDG_CONFIG_HOME/cursor`; else `~/.cursor`. Without honoring those, a user who
/// sets one has Cursor read the overridden dir while pixtuoid wrote `~/.cursor`
/// (installed, but no sprite). The home base is `USERPROFILE`-first (Node
/// `os.homedir`), NOT the Electron IDE's `%APPDATA%\Cursor`.
pub(crate) fn default_config_path() -> Result<PathBuf> {
    cursor_config_dir()
        .map(|d| d.join("hooks.json"))
        .ok_or_else(|| io::home_unset("the home directory", "HOME/USERPROFILE"))
}

fn cursor_config_dir() -> Option<PathBuf> {
    resolve_config_dir(
        pixtuoid_core::platform::path_env("CURSOR_CONFIG_DIR"),
        pixtuoid_core::platform::path_env("XDG_CONFIG_HOME"),
        cfg!(all(unix, not(target_os = "macos"))),
        pixtuoid_core::platform::user_home_opt(),
    )
}

/// Pure core for [`cursor_config_dir`] — env overrides, the Linux/BSD XDG flag, and
/// the home are injected so every arm unit-tests on any host.
fn resolve_config_dir(
    cursor_config_dir_env: Option<PathBuf>,
    xdg_config_home_env: Option<PathBuf>,
    xdg_applies: bool,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(d) = cursor_config_dir_env {
        return Some(d);
    }
    if let Some(xdg) = xdg_config_home_env.filter(|_| xdg_applies) {
        return Some(xdg.join("cursor"));
    }
    home.map(|h| h.join(".cursor"))
}

/// Presence probe for auto-detection. Cursor never creates `hooks.json` itself (it
/// is purely user-authored), so a file-exists check on it would never fire — probe
/// Cursor's own config dir (created on first run) instead.
pub(crate) fn detect_installed() -> bool {
    cursor_config_dir().is_some_and(|d| d.exists()) || io::home_relative(".cursor").exists()
}

/// Cursor runs the `command` under a shell, so the OS forms are
/// [`crate::install::hook_cmd::shell_hook_command`]'s. Err on non-UTF-8 (prevents
/// the to_string_lossy dead-hook).
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    let p = merge::hook_path_str(resolved)?;
    crate::install::hook_cmd::shell_hook_command(p, "cursor")
}

pub(crate) fn merge_install(content: &str, hook_cmd: &str) -> Result<MergeOutcome> {
    merge::flat_json_merge_outcome_install(content, "hooks.json", |doc| {
        json_merge_install(doc, hook_cmd)
    })
}

pub(crate) fn merge_uninstall(content: &str) -> Result<MergeOutcome> {
    // `version` is deliberately left in place: we can't tell our set-if-absent `1`
    // from a user's own value, and stripping it would DELETE a hookless
    // `{"version": N}` a user wrote themselves.
    merge::flat_json_uninstall(content)
}

fn managed_entry(hook_command: &str) -> Value {
    json!({
        SENTINEL_KEY: true,
        "command": hook_command
    })
}

/// Install-schema verification — Cursor's flat-JSON shape PLUS the Cursor-specific
/// whole-file gate: a hooks.json with intact managed entries but no numeric
/// top-level `version` loads NO hooks at all (the silent-dead class).
pub(crate) fn verify_schema(content: &str) -> crate::install::verify::SchemaParse {
    let mut parse = crate::install::verify::flat_json_verify(content, CURSOR_EVENTS, SENTINEL_KEY);
    // Only reachable on parseable JSON — an unparseable file is already a HARD
    // "no longer parses" issue from `flat_json_verify`.
    if let Ok(doc) = serde_json::from_str::<Value>(content)
        && !doc.get("version").is_some_and(|v| v.is_number())
    {
        parse.issues.push(
            "hooks.json has no numeric top-level `version` key — Cursor requires it, \
                 so no hooks load (reconnect via the Sources panel)"
                .to_string(),
        );
    }
    parse
}

fn json_merge_install(doc: Value, hook_command: &str) -> Value {
    // Cursor requires a `version`; set it if absent, preserve a user's value.
    let mut root: Map<String, Value> = doc.as_object().cloned().unwrap_or_default();
    root.entry("version".to_string())
        .or_insert_with(|| json!(1));
    merge::flat_json_merge_install(
        Value::Object(root),
        CURSOR_EVENTS,
        SENTINEL_KEY,
        managed_entry,
        hook_command,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_honors_cursor_config_dir_then_xdg_on_linux_else_dot_cursor() {
        assert_eq!(
            resolve_config_dir(
                Some("/custom/cur".into()),
                Some("/xdg".into()),
                true,
                Some("/h".into())
            ),
            Some(PathBuf::from("/custom/cur"))
        );
        assert_eq!(
            resolve_config_dir(None, Some("/xdg".into()), true, Some("/home/u".into())),
            Some(PathBuf::from("/xdg").join("cursor"))
        );
        assert_eq!(
            resolve_config_dir(None, None, true, Some("/home/u".into())),
            Some(PathBuf::from("/home/u").join(".cursor"))
        );
        assert_eq!(
            resolve_config_dir(
                None,
                Some("/xdg".into()),
                false,
                Some(r"C:\Users\me".into())
            ),
            Some(PathBuf::from(r"C:\Users\me").join(".cursor"))
        );
        assert_eq!(resolve_config_dir(None, None, false, None), None);
    }

    fn json_merge_uninstall(doc: Value) -> Value {
        merge::flat_json_merge_uninstall(doc, SENTINEL_KEY)
    }

    #[test]
    fn install_creates_flat_entries_for_all_events_with_version() {
        let doc = json_merge_install(json!({}), "PIXTUOID_SOURCE=cursor '/opt/pixtuoid-hook'");
        assert_eq!(doc["version"], json!(1), "Cursor requires a version field");
        let hooks = doc.get("hooks").and_then(|v| v.as_object()).unwrap();
        for ev in CURSOR_EVENTS {
            let arr = hooks.get(*ev).and_then(|v| v.as_array()).unwrap();
            assert_eq!(arr.len(), 1, "event {ev}");
            let entry = &arr[0];
            assert_eq!(
                entry["command"].as_str().unwrap(),
                "PIXTUOID_SOURCE=cursor '/opt/pixtuoid-hook'"
            );
            assert!(entry[SENTINEL_KEY].as_bool().unwrap());
            assert!(
                entry.get("hooks").is_none() && entry.get("type").is_none(),
                "must not write CC-style nested groups"
            );
        }
    }

    #[test]
    fn install_preserves_existing_version() {
        let doc = json_merge_install(json!({"version": 2}), "/x");
        assert_eq!(
            doc["version"],
            json!(2),
            "must not clobber a user's version"
        );
    }

    #[test]
    fn install_is_idempotent_and_replaces_across_paths() {
        let a = json_merge_install(json!({}), "PIXTUOID_SOURCE=cursor '/opt/a/pixtuoid-hook'");
        let b = json_merge_install(a.clone(), "PIXTUOID_SOURCE=cursor '/opt/a/pixtuoid-hook'");
        assert_eq!(a, b, "same command re-install is a no-op");
        let c = json_merge_install(a, "PIXTUOID_SOURCE=cursor '/opt/b/pixtuoid-hook'");
        for ev in CURSOR_EVENTS {
            assert_eq!(
                c["hooks"][*ev].as_array().unwrap().len(),
                1,
                "event {ev} duplicated on path change"
            );
        }
    }

    #[test]
    fn install_preserves_user_entries() {
        let initial = json!({
            "version": 1,
            "hooks": {"preToolUse": [ { "command": "my-guard.sh" } ]},
            "other": "setting"
        });
        let merged = json_merge_install(initial, "/x");
        let arr = merged["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["command"], json!("my-guard.sh"));
        assert_eq!(merged["other"], json!("setting"));
    }

    #[test]
    fn uninstall_removes_only_managed_entries_and_empty_maps() {
        let installed = json_merge_install(
            json!({"hooks": {"preToolUse": [ { "command": "my-guard.sh" } ]}}),
            "/x",
        );
        let cleaned = json_merge_uninstall(installed);
        let arr = cleaned["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["command"], json!("my-guard.sh"));
        for ev in CURSOR_EVENTS.iter().filter(|e| **e != "preToolUse") {
            assert!(
                cleaned["hooks"].get(*ev).is_none(),
                "event {ev} should be dropped once empty"
            );
        }
        assert_eq!(cleaned["version"], json!(1));
    }

    #[test]
    fn uninstall_all_managed_drops_hooks_but_keeps_version() {
        let installed = json_merge_install(json!({}), "/x");
        let cleaned = json_merge_uninstall(installed);
        assert!(cleaned.get("hooks").is_none(), "got {cleaned}");
        assert_eq!(cleaned["version"], json!(1), "got {cleaned}");
    }

    #[test]
    fn uninstall_preserves_a_users_version_only_file() {
        let installed = json_merge_install(json!({"version": 3}), "/x");
        let cleaned = json_merge_uninstall(installed);
        assert_eq!(
            cleaned,
            json!({"version": 3}),
            "a user's version must not be lost on uninstall: {cleaned}"
        );
    }

    #[test]
    fn verify_schema_flags_a_missing_version_and_passes_full_install() {
        let installed =
            json_merge_install(json!({}), "PIXTUOID_SOURCE=cursor '/opt/pixtuoid-hook'");
        let sound = verify_schema(&installed.to_string());
        assert!(sound.issues.is_empty(), "{:?}", sound.issues);

        let mut versionless = installed.clone();
        versionless.as_object_mut().unwrap().remove("version");
        let p = verify_schema(&versionless.to_string());
        assert!(
            p.issues.iter().any(|i| i.contains("version")),
            "a version-less hooks.json must be flagged: {:?}",
            p.issues
        );

        let mut stringly = installed;
        stringly["version"] = json!("1");
        let p = verify_schema(&stringly.to_string());
        assert!(
            p.issues.iter().any(|i| i.contains("version")),
            "a non-numeric version must be flagged: {:?}",
            p.issues
        );
    }

    #[test]
    fn merge_install_idempotent_reports_unchanged() {
        let first = merge_install("", "/x").unwrap();
        assert!(first.changed);
        let second = merge_install(&first.content, "/x").unwrap();
        assert!(!second.changed, "second install is a semantic no-op");
    }

    #[test]
    fn merge_uninstall_no_pixtuoid_hooks_reports_unchanged() {
        let user = r#"{ "version": 1, "hooks": { "stop": [ { "command": "notify done" } ] } }"#;
        let out = merge_uninstall(user).unwrap();
        assert!(!out.changed, "no managed entries → semantic no-op");
    }

    #[test]
    fn merge_install_rejects_valid_json_that_is_not_an_object() {
        assert!(merge_install("[1, 2, 3]", "/x").is_err());
        assert!(merge_install("42", "/x").is_err());
    }

    #[test]
    fn merge_install_rejects_invalid_json() {
        assert!(merge_install("{not json", "/x").is_err());
    }

    #[test]
    fn install_coerces_non_object_hooks_and_non_array_events() {
        let doc = json_merge_install(json!({"hooks": "garbage"}), "/x");
        assert!(doc["hooks"].is_object());
        let doc = json_merge_install(json!({"hooks": {"stop": 42}}), "/x");
        assert_eq!(doc["hooks"]["stop"].as_array().unwrap().len(), 1);
    }

    // Unix-only: on Windows the bare form is emitted and this spaced path is REJECTED.
    #[cfg(unix)]
    #[test]
    fn hook_command_stamps_source_and_quotes() {
        let cmd = hook_command(Path::new("/Users/Jane Doe/bin/pixtuoid-hook"), false).unwrap();
        assert_eq!(
            cmd,
            "PIXTUOID_SOURCE=cursor '/Users/Jane Doe/bin/pixtuoid-hook'"
        );
    }

    #[test]
    #[cfg(windows)]
    fn hook_command_emits_bare_exec_form_with_source_flag_on_windows() {
        let cmd = hook_command(Path::new(r"C:\tools\pixtuoid-hook.exe"), false).unwrap();
        assert_eq!(cmd, r"C:\tools\pixtuoid-hook.exe --source cursor");
    }

    #[test]
    #[cfg(windows)]
    fn hook_command_rejects_cmd_unsafe_path_on_windows() {
        assert!(hook_command(Path::new(r"C:\Program Files\pixtuoid-hook.exe"), false).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn hook_command_errors_on_non_utf8_path() {
        use std::os::unix::ffi::OsStrExt;
        let bad = Path::new(std::ffi::OsStr::from_bytes(b"/x/\xff/pixtuoid-hook"));
        assert!(hook_command(bad, false).is_err());
    }

    #[test]
    fn every_registered_cursor_event_decodes() {
        use pixtuoid_core::source::decoder::decode_hook_payload;
        for ev in CURSOR_EVENTS {
            let payload = serde_json::json!({
                "hook_event_name": ev,
                "session_id": "s",
                "cwd": "/repo",
                "_pixtuoid_source": "cursor",
            });
            assert!(
                decode_hook_payload(payload).is_ok(),
                "registered Cursor hook {ev:?} has no decoder arm — it would bail \
                 as unsupported. Add an arm in pixtuoid-core source/cursor.rs."
            );
        }
    }

    #[test]
    fn cursor_events_pins_the_exact_registered_set() {
        crate::install::assert_event_roster(
            "CURSOR_EVENTS",
            CURSOR_EVENTS,
            &[
                "sessionStart",
                "preToolUse",
                "postToolUse",
                "postToolUseFailure",
                "stop",
                "sessionEnd",
            ],
        );
    }
}
