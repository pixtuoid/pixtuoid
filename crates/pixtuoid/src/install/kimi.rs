//! Kimi Code CLI hook install target.
//!
//! Writes the GLOBAL Kimi config (`<KIMI_CODE_HOME>/config.toml`, default
//! `~/.kimi-code/config.toml`) — Kimi's documented hooks live in a top-level
//! `[[hooks]]` array of tables.
//!
//! Load-bearing details:
//! - **NO `_pixtuoid` sentinel.** Kimi's `[[hooks]]` allows EXACTLY four fields
//!   (`event`/`matcher`/`command`/`timeout`) and extra fields fail the config
//!   load, so managed-entry detection keys on the **command's source marker**
//!   instead (see [`is_managed_command`]), which is path-independent so it still
//!   matches after a shim-path change.
//! - **One command for all events.** Kimi's shim reads the event off stdin, so
//!   every `[[hooks]]` entry carries the identical command. `matcher` is OMITTED
//!   (match every tool).
//! - **Shell execution.** Kimi runs the `command` under a shell, so the OS forms
//!   mirror Codex/CodeWhale via `hook_cmd::shell_hook_command`. (CAPTURE-GATED,
//!   like Cursor: if a live run proves Kimi argv-execs the command WITHOUT a
//!   shell, switch to `exec_hook_command` — the Hermes form.)

use std::path::{Path, PathBuf};

use anyhow::Result;
use toml::value::Table;

use crate::install::io;
use crate::install::target::MergeOutcome;
use pixtuoid_core::source::kimi::SOURCE_NAME;

/// Events we register == events we decode, enforced by
/// `every_registered_kimi_event_decodes`. The two `*Failure` variants close a
/// failed tool/turn via the source's custom `Extend` decoder — a failed tool
/// fires `PostToolUseFailure`, which must still end the activity.
pub(crate) const KIMI_EVENTS: &[&str] = &[
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// Per-entry `timeout` (seconds). Kimi fails OPEN on a non-zero/timeout, so this
/// only bounds a pathologically hung shim on the blocking-capable events
/// (`PreToolUse`/`Stop`). Kimi's accepted range is 1–600.
const KIMI_HOOK_TIMEOUT_SECS: i64 = 5;

/// The data-root dir Kimi actually reads: `$KIMI_CODE_HOME` verbatim (the ONE
/// documented override; no `KIMI_HOME`/`KIMI_CONFIG_DIR`), else
/// `<home>/.kimi-code`. Env is taken verbatim (no `~`-expand): the documented
/// value is already shell-expanded/absolute.
fn kimi_config_dir() -> Option<PathBuf> {
    resolve_config_dir(
        pixtuoid_core::platform::path_env("KIMI_CODE_HOME"),
        pixtuoid_core::platform::user_home_opt(),
    )
}

/// Pure core for [`kimi_config_dir`], with the env override and home injected so
/// both arms unit-test without env/FS mutation.
fn resolve_config_dir(
    kimi_code_home_env: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(h) = kimi_code_home_env {
        return Some(h);
    }
    home.map(|h| h.join(".kimi-code"))
}

pub(crate) fn default_config_path() -> Result<PathBuf> {
    kimi_config_dir()
        .map(|d| d.join("config.toml"))
        .ok_or_else(|| io::home_unset("the home directory", "HOME/USERPROFILE"))
}

/// Presence probe for auto-detection. Kimi may not create `config.toml` until the
/// user configures a provider, but it creates its data root on first run — probe
/// that dir, not the file we write.
pub(crate) fn detect_installed() -> bool {
    kimi_config_dir().is_some_and(|d| d.exists())
}

/// Kimi runs the `command` under a shell (module doc), so the OS forms are
/// [`crate::install::hook_cmd::shell_hook_command`]'s. Err on non-UTF-8 (prevents
/// the to_string_lossy dead-hook).
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    let p = crate::install::merge::hook_path_str(resolved)?;
    crate::install::hook_cmd::shell_hook_command(p, SOURCE_NAME)
}

pub(crate) fn merge_install(content: &str, hook_cmd: &str) -> Result<MergeOutcome> {
    crate::install::merge::toml_merge_outcome(content, |doc| toml_merge_install(doc, hook_cmd))
}

pub(crate) fn merge_uninstall(content: &str) -> Result<MergeOutcome> {
    crate::install::merge::toml_merge_outcome(content, toml_merge_uninstall)
}

/// The `PIXTUOID_SOURCE=kimi` env-prefix marker (Unix `shell_hook_command` form).
fn env_marker() -> String {
    format!("PIXTUOID_SOURCE={SOURCE_NAME}")
}

/// The ` --source kimi` flag marker (Windows bare form + any `--source` form).
fn flag_marker() -> String {
    format!("{}{SOURCE_NAME}", crate::install::hook_cmd::SOURCE_FLAG)
}

/// A `[[hooks]]` entry is OURS iff its `command` carries the kimi source marker.
/// Kimi's config forbids the `_pixtuoid` sentinel (module doc), so this
/// command-substring test replaces it. Both platform forms are checked so a
/// config synced across OSes still round-trips. Keyed on the SOURCE-specific
/// marker, not the shim basename, so it never removes a different pixtuoid
/// source's entry.
fn is_managed_command(command: &str) -> bool {
    command.contains(&env_marker()) || command.contains(&flag_marker())
}

fn is_managed_entry(entry: &toml::Value) -> bool {
    entry
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(is_managed_command)
}

fn managed_entry(event: &str, hook_cmd: &str) -> toml::Value {
    let mut t = Table::new();
    t.insert("event".into(), toml::Value::String(event.into()));
    t.insert("command".into(), toml::Value::String(hook_cmd.into()));
    t.insert(
        "timeout".into(),
        toml::Value::Integer(KIMI_HOOK_TIMEOUT_SECS),
    );
    toml::Value::Table(t)
}

fn toml_merge_install(doc: toml::Value, hook_cmd: &str) -> toml::Value {
    let mut root = doc.as_table().cloned().unwrap_or_default();
    let arr = root
        .entry("hooks".to_string())
        .or_insert_with(|| toml::Value::Array(vec![]));
    if !arr.is_array() {
        *arr = toml::Value::Array(vec![]);
    }
    if let Some(arr) = arr.as_array_mut() {
        arr.retain(|e| !is_managed_entry(e));
        for ev in KIMI_EVENTS {
            arr.push(managed_entry(ev, hook_cmd));
        }
    }
    toml::Value::Table(root)
}

fn toml_merge_uninstall(mut doc: toml::Value) -> toml::Value {
    let Some(root) = doc.as_table_mut() else {
        return doc;
    };
    if let Some(arr) = root.get_mut("hooks").and_then(|h| h.as_array_mut()) {
        arr.retain(|e| !is_managed_entry(e));
    }
    if root
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|a| a.is_empty())
    {
        root.remove("hooks");
    }
    doc
}

/// Install-schema verification: every `KIMI_EVENTS` event still has a managed
/// `[[hooks]]` entry (detected by the command marker, not a sentinel), and the
/// shim path extracted for `install::verify_target` to stat.
pub(crate) fn verify_schema(content: &str) -> crate::install::verify::SchemaParse {
    use crate::install::verify::{SchemaParse, ShimRef, assemble, shell_shim_ref};
    let Ok(doc) = toml::from_str::<toml::Value>(content) else {
        return SchemaParse::broken("config.toml no longer parses as TOML");
    };
    let entries: Vec<&toml::Value> = doc
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|a| a.iter().filter(|e| is_managed_entry(e)).collect())
        .unwrap_or_default();
    // The shared `assemble` no-managed message names the `_pixtuoid` sentinel
    // Kimi can't carry, so word this one here.
    if entries.is_empty() {
        return SchemaParse::broken(
            "no managed pixtuoid hook entries in [[hooks]] (the pixtuoid source marker \
             `PIXTUOID_SOURCE=kimi` / `--source kimi` is absent — the config was hand-edited \
             or hooks were never installed)",
        );
    }
    let mut missing = Vec::new();
    let mut shim = ShimRef::Unknown;
    for ev in KIMI_EVENTS {
        match entries
            .iter()
            .find(|e| e.get("event").and_then(|v| v.as_str()) == Some(ev))
        {
            Some(e) => {
                if shim == ShimRef::Unknown {
                    shim = e
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(shell_shim_ref)
                        .unwrap_or(ShimRef::Unknown);
                }
            }
            None => missing.push(*ev),
        }
    }
    assemble(&missing, true, shim, vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> toml::Value {
        toml::from_str(s).unwrap()
    }

    const CMD: &str = "PIXTUOID_SOURCE=kimi '/opt/bin/pixtuoid-hook'";

    #[test]
    fn config_dir_honors_kimi_code_home_then_default() {
        assert_eq!(
            resolve_config_dir(Some("/custom/kimi".into()), Some("/home/u".into())),
            Some(PathBuf::from("/custom/kimi"))
        );
        assert_eq!(
            resolve_config_dir(None, Some("/home/u".into())),
            Some(PathBuf::from("/home/u").join(".kimi-code"))
        );
        assert_eq!(resolve_config_dir(None, None), None);
    }

    #[test]
    fn default_config_path_honors_kimi_code_home_env() {
        // Unconditional (not `if let Ok`) so a mutation making default_config_path
        // always-Err is CAUGHT, not skipped.
        let custom = std::env::temp_dir().join("pixtuoid-kimi-home-cfg-test");
        temp_env::with_var("KIMI_CODE_HOME", Some(&custom), || {
            assert_eq!(default_config_path().unwrap(), custom.join("config.toml"));
        });

        // Assert only the filename when a home resolves — a stripped env
        // legitimately errs.
        temp_env::with_var("KIMI_CODE_HOME", Some(""), || {
            if let Ok(p) = default_config_path() {
                assert_eq!(p.file_name().and_then(|n| n.to_str()), Some("config.toml"));
            }
        });
    }

    #[test]
    fn detect_installed_probes_the_data_root_not_the_config_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("kimi");
        temp_env::with_var("KIMI_CODE_HOME", Some(&root), || {
            assert!(!detect_installed(), "an absent data root must not detect");

            std::fs::create_dir_all(&root).unwrap();
            assert!(
                detect_installed(),
                "an existing data root must detect even with no config.toml"
            );
        });
    }

    #[test]
    fn install_creates_one_entry_per_event_with_command_and_timeout() {
        let out = merge_install("", CMD).unwrap();
        assert!(out.changed);
        let v = parse(&out.content);
        let arr = v["hooks"].as_array().unwrap();
        assert_eq!(arr.len(), KIMI_EVENTS.len());
        for (entry, ev) in arr.iter().zip(KIMI_EVENTS) {
            assert_eq!(entry["event"].as_str().unwrap(), *ev);
            assert_eq!(entry["command"].as_str().unwrap(), CMD);
            assert_eq!(
                entry["timeout"].as_integer().unwrap(),
                KIMI_HOOK_TIMEOUT_SECS
            );
            assert!(
                entry.get("_pixtuoid").is_none(),
                "must NOT write a sentinel (Kimi rejects extra fields)"
            );
            assert!(is_managed_entry(entry), "our entry must be self-detecting");
        }
    }

    #[test]
    fn install_is_idempotent_and_replaces_across_paths() {
        let a = merge_install("", CMD).unwrap();
        let b = merge_install(&a.content, CMD).unwrap();
        assert!(!b.changed, "same-command re-install is a semantic no-op");
        let c = merge_install(
            &a.content,
            "PIXTUOID_SOURCE=kimi '/usr/local/bin/pixtuoid-hook'",
        )
        .unwrap();
        assert_eq!(
            parse(&c.content)["hooks"].as_array().unwrap().len(),
            KIMI_EVENTS.len(),
            "path change must not duplicate entries"
        );
    }

    #[test]
    fn install_preserves_user_hooks_and_other_keys() {
        let user = r#"
default_model = "kimi-for-coding"

[providers.moonshot]
api_key = "secret"

[[hooks]]
event = "Notification"
command = "terminal-notifier -message done"
"#;
        let out = merge_install(user, CMD).unwrap();
        let v = parse(&out.content);
        assert_eq!(v["default_model"].as_str(), Some("kimi-for-coding"));
        assert_eq!(
            v["providers"]["moonshot"]["api_key"].as_str(),
            Some("secret"),
            "unrelated provider/key config survives"
        );
        let arr = v["hooks"].as_array().unwrap();
        assert_eq!(arr.len(), 1 + KIMI_EVENTS.len());
        assert!(
            arr.iter()
                .any(|e| e["command"].as_str() == Some("terminal-notifier -message done")),
            "the user's own hook must be preserved"
        );
    }

    #[test]
    fn uninstall_removes_only_managed_entries() {
        let user = "[[hooks]]\nevent = \"Notification\"\ncommand = \"my-own-hook\"\n";
        let installed = merge_install(user, CMD).unwrap();
        let cleaned = merge_uninstall(&installed.content).unwrap();
        assert!(cleaned.changed);
        let v = parse(&cleaned.content);
        let arr = v["hooks"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "only the user's own hook remains");
        assert_eq!(arr[0]["command"].as_str(), Some("my-own-hook"));
    }

    #[test]
    fn uninstall_of_pixtuoid_only_install_drops_the_hooks_array() {
        let installed = merge_install("", CMD).unwrap();
        let cleaned = merge_uninstall(&installed.content).unwrap();
        let v = parse(&cleaned.content);
        assert!(
            v.get("hooks").is_none(),
            "a pixtuoid-only [[hooks]] must be fully removed, got {v}"
        );
    }

    #[test]
    fn uninstall_no_managed_hooks_is_a_noop() {
        let user = "[[hooks]]\nevent = \"Notification\"\ncommand = \"echo hi\"\n";
        let out = merge_uninstall(user).unwrap();
        assert!(!out.changed, "no managed entries → semantic no-op");
    }

    #[test]
    fn merge_install_rejects_invalid_toml() {
        assert!(merge_install("not = valid = toml", CMD).is_err());
    }

    #[test]
    fn install_coerces_a_non_array_hooks_key() {
        let out = merge_install("hooks = \"garbage\"\n", CMD).unwrap();
        let v = parse(&out.content);
        assert!(v["hooks"].is_array());
        assert_eq!(v["hooks"].as_array().unwrap().len(), KIMI_EVENTS.len());
    }

    #[test]
    fn verify_schema_passes_full_install_and_flags_missing_and_absent() {
        use crate::install::verify::ShimRef;
        let full = merge_install("", CMD).unwrap().content;
        let sound = verify_schema(&full);
        assert!(sound.issues.is_empty(), "{:?}", sound.issues);
        assert_ne!(sound.shim, ShimRef::Unknown);
        assert_eq!(
            sound.shim,
            ShimRef::Absolute(PathBuf::from("/opt/bin/pixtuoid-hook"))
        );

        let partial = "[[hooks]]\nevent = \"SessionStart\"\ncommand = \"PIXTUOID_SOURCE=kimi '/x/pixtuoid-hook'\"\n";
        let p = verify_schema(partial);
        let joined = p.issues.join(" | ");
        assert!(
            joined.contains("missing hook entries for") && joined.contains("PreToolUse"),
            "a partial install must list missing events, got {:?}",
            p.issues
        );

        let none = verify_schema("[[hooks]]\nevent = \"Notification\"\ncommand = \"echo hi\"\n");
        assert!(
            none.issues.iter().any(|i| i.contains("--source kimi")),
            "the no-managed message must name the command marker, not a sentinel, got {:?}",
            none.issues
        );
    }

    #[test]
    fn verify_schema_reports_broken_on_unparseable_toml() {
        use crate::install::verify::ShimRef;
        let res = verify_schema("not = = toml");
        assert_eq!(res.shim, ShimRef::Unknown);
        assert!(
            res.issues
                .iter()
                .any(|i| i.contains("no longer parses as TOML"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn hook_command_is_the_env_prefix_shell_form() {
        let cmd = hook_command(Path::new("/opt/bin/pixtuoid-hook"), false).unwrap();
        assert_eq!(cmd, "PIXTUOID_SOURCE=kimi '/opt/bin/pixtuoid-hook'");
        assert!(
            is_managed_command(&cmd),
            "the written command must self-detect"
        );
    }

    #[test]
    #[cfg(windows)]
    fn hook_command_emits_bare_exec_form_with_source_flag_on_windows() {
        let cmd = hook_command(Path::new(r"C:\tools\pixtuoid-hook.exe"), false).unwrap();
        assert_eq!(cmd, r"C:\tools\pixtuoid-hook.exe --source kimi");
        assert!(
            is_managed_command(&cmd),
            "the Windows form must self-detect"
        );
    }

    #[test]
    #[cfg(unix)]
    fn hook_command_errors_on_non_utf8_path() {
        use std::os::unix::ffi::OsStrExt;
        let bad = Path::new(std::ffi::OsStr::from_bytes(b"/x/\xff/pixtuoid-hook"));
        assert!(hook_command(bad, false).is_err());
    }

    #[test]
    fn managed_command_detects_both_platform_forms_and_ignores_foreign() {
        assert!(is_managed_command(
            "PIXTUOID_SOURCE=kimi '/opt/pixtuoid-hook'"
        ));
        assert!(is_managed_command(
            r"C:\bin\pixtuoid-hook.exe --source kimi"
        ));
        assert!(!is_managed_command(
            "PIXTUOID_SOURCE=codex '/opt/pixtuoid-hook'"
        ));
        assert!(!is_managed_command(
            r"C:\bin\pixtuoid-hook.exe --source cursor"
        ));
        assert!(!is_managed_command("terminal-notifier -message done"));
    }

    #[test]
    fn every_registered_kimi_event_decodes() {
        use pixtuoid_core::source::decoder::decode_hook_payload;
        for ev in KIMI_EVENTS {
            let payload = serde_json::json!({
                "hook_event_name": ev,
                "session_id": "s",
                "cwd": "/repo",
                "tool_name": "Bash",
                "tool_call_id": "t1",
                "_pixtuoid_source": SOURCE_NAME,
            });
            assert!(
                decode_hook_payload(payload).is_ok(),
                "registered Kimi hook {ev:?} has no decoder arm — it would bail as \
                 unsupported. Add an arm in pixtuoid-core source/kimi.rs (or the shared arms)."
            );
        }
    }

    // MEMBERSHIP pin — the completeness half `every_registered_kimi_event_decodes`
    // can't see; the two `*Failure` variants are serviced ONLY by the custom
    // Extend decoder, so update this pin deliberately.
    #[test]
    fn kimi_events_pins_the_exact_registered_set() {
        crate::install::assert_event_roster(
            "KIMI_EVENTS",
            KIMI_EVENTS,
            &[
                "SessionStart",
                "PreToolUse",
                "PostToolUse",
                "PostToolUseFailure",
                "PermissionRequest",
                "Stop",
                "StopFailure",
                "SessionEnd",
            ],
        );
    }
}
