//! CodeWhale hook install target.
//!
//! The `[hooks]` table holds a single `hooks` ARRAY of `{event, command}`
//! entries — NOT Codex's per-event group keys, NOT Claude's nested
//! `{matcher, hooks}`.
//!
//! - **Per-event command.** CodeWhale sets no event env var, so the event name
//!   is BAKED into each entry's command as ` --event <name>`; `hook_command`
//!   returns the BASE command and `merge_install` appends the suffix.
//! - **`enabled = true`.** CodeWhale gates ALL hooks on `[hooks].enabled`
//!   (absent defaults true).
//! - **`_pixtuoid` sentinel.** CodeWhale's `Hook` serde has no
//!   `deny_unknown_fields`, so the marker round-trips; managed-entry detection
//!   keys on it (the per-event command's last token is the event name, not the
//!   binary, so a Codex-style command-basename fallback wouldn't apply).

use std::path::{Path, PathBuf};

use anyhow::Result;
use toml::value::Table;

use crate::install::SENTINEL_KEY;
use crate::install::io;
use crate::install::target::MergeOutcome;

/// Events we register == events we decode (`source/codewhale.rs`). The `bool` is
/// `env_mode`: `true` events carry identity via `DEEPSEEK_*` env vars, so their
/// command bakes `--event <name>` and the shim builds the envelope from env;
/// `false` events (the subagent observer hooks) are forwarded RAW on stdin.
///
/// turn_end / mode_change / on_error / shell_env are deliberately absent
/// (per-turn noise / no lifecycle meaning). CodeWhale has NO approval hook in the
/// TUI path, so there is no Waiting event to register — no signal exists.
pub(crate) const CODEWHALE_EVENTS: &[(&str, bool)] = &[
    ("session_start", true),
    ("message_submit", true),
    ("tool_call_before", true),
    ("tool_call_after", true),
    ("session_end", true),
    ("subagent_spawn", false),
    ("subagent_complete", false),
];

/// The config CodeWhale actually reads: the `CODEWHALE_CONFIG_PATH` /
/// `DEEPSEEK_CONFIG_PATH` overrides (each a FULL file path), else
/// `$CODEWHALE_HOME/config.toml`, else `<OS home>/.codewhale/config.toml`, else
/// the legacy `<OS home>/.deepseek/config.toml` when only that exists — writing a fresh
/// `.codewhale/config.toml` would make CodeWhale PREFER our near-empty file and
/// drop the user's provider/key config.
///
/// The OS home comes from [`cw_home`] (`HOME` FIRST), NOT pixtuoid's generic
/// `USERPROFILE`-first resolver: CodeWhale reads a set `$HOME` first, so a
/// Windows user who exports `HOME` (Git Bash / MSYS2 / Cygwin) would otherwise get
/// the hooks written to a file CodeWhale never loads (installed, but no sprite).
///
/// # Errors
///
/// If an override is still relative once its `~` is expanded, as CodeWhale
/// refuses it, or no home resolves.
pub(crate) fn default_config_path() -> Result<PathBuf> {
    let home = cw_home();
    resolve_config_path(
        cw_env("CODEWHALE_CONFIG_PATH", home.as_deref())?,
        cw_env("DEEPSEEK_CONFIG_PATH", home.as_deref())?,
        cw_env("CODEWHALE_HOME", home.as_deref())?,
        home.clone(),
        |p| p.exists(),
    )
}

/// The OS home as CodeWhale resolves it (`crates/paths/src/lib.rs` `user_home`):
/// a set `HOME` decides alone and must be absolute, else `USERPROFILE`, else
/// Windows' `HOMEDRIVE` + `HOMEPATH`, each absolute. Not mirrored: its last
/// `dirs::home_dir` passwd lookup.
fn cw_home() -> Option<PathBuf> {
    use pixtuoid_core::platform::path_env_trimmed;
    if let Some(home) = path_env_trimmed("HOME") {
        return home.is_absolute().then_some(home);
    }
    path_env_trimmed("USERPROFILE")
        .filter(|p| p.is_absolute())
        .or_else(|| {
            let mut p = path_env_trimmed("HOMEDRIVE").filter(|_| cfg!(windows))?;
            p.push(path_env_trimmed("HOMEPATH")?);
            Some(p).filter(|p| p.is_absolute())
        })
}

/// A CodeWhale path variable as CodeWhale reads it (`crates/paths/src/lib.rs`
/// `absolute_path_env`): blank is unset, text is trimmed, a leading `~`
/// expands against the OS home, and a path still relative is refused.
fn cw_env(name: &str, home: Option<&Path>) -> Result<Option<PathBuf>> {
    let Some(raw) = pixtuoid_core::platform::path_env_trimmed(name) else {
        return Ok(None);
    };
    // Upstream expands only text; other bytes stay verbatim, and so relative.
    let path = io::expand_tilde(&raw, home.filter(|_| raw.to_str().is_some()));
    anyhow::ensure!(
        path.is_absolute(),
        "{name} must be an absolute path, got {}",
        path.display()
    );
    Ok(Some(path))
}

/// Pure core for [`default_config_path`] — env overrides, the resolved OS home,
/// and the existence check are all injected so every arm unit-tests without
/// env/FS mutation. `codewhale_home_env` is the `.codewhale`-EQUIVALENT app dir
/// verbatim (no `.codewhale` join — that's how CodeWhale uses it).
fn resolve_config_path(
    codewhale_config_env: Option<PathBuf>,
    deepseek_config_env: Option<PathBuf>,
    codewhale_home_env: Option<PathBuf>,
    os_home: Option<PathBuf>,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf> {
    if let Some(p) = codewhale_config_env {
        return Ok(p);
    }
    if let Some(p) = deepseek_config_env {
        return Ok(p);
    }
    // An explicit CODEWHALE_HOME is final (`crates/config/src/lib.rs`
    // `default_config_path`, `codewhale_home_is_explicit`).
    if let Some(h) = codewhale_home_env {
        return Ok(h.join("config.toml"));
    }
    let Some(home) = os_home else {
        return Err(io::home_unset(
            "CodeWhale's home",
            "CODEWHALE_CONFIG_PATH/DEEPSEEK_CONFIG_PATH/CODEWHALE_HOME/HOME/USERPROFILE",
        ));
    };
    let modern = home.join(".codewhale").join("config.toml");
    let legacy = home.join(".deepseek").join("config.toml");
    Ok(if !exists(&modern) && exists(&legacy) {
        legacy
    } else {
        modern
    })
}

/// Presence probe for auto-detection. CodeWhale's config FILE may be absent on a
/// fresh install while the product-state dir exists, and the legacy `~/.deepseek`
/// layout puts config elsewhere — so probe the state dirs (created by CodeWhale on
/// first launch) rather than the file we write.
pub(crate) fn detect_installed() -> bool {
    let home = cw_home();
    // An explicit CODEWHALE_HOME suppresses the legacy dir (`legacy_deepseek_home`);
    // one CodeWhale refuses is no install.
    match cw_env("CODEWHALE_HOME", home.as_deref()) {
        Ok(Some(h)) => h.exists(),
        Ok(None) => {
            home.is_some_and(|h| h.join(".codewhale").exists() || h.join(".deepseek").exists())
        }
        Err(_) => false,
    }
}

/// The BASE hook command — no `--event`, which `merge_install` appends per event.
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    // `_explicit` is Claude's bare-name-vs-absolute switch — CodeWhale always
    // embeds the absolute path, so the flag changes nothing here.
    let p = crate::install::merge::hook_path_str(resolved)?;
    crate::install::hook_cmd::shell_hook_command(p, "codewhale")
}

pub(crate) fn merge_install(content: &str, base_cmd: &str) -> Result<MergeOutcome> {
    crate::install::merge::toml_merge_outcome(content, |doc| toml_merge_install(doc, base_cmd))
}

pub(crate) fn merge_uninstall(content: &str) -> Result<MergeOutcome> {
    crate::install::merge::toml_merge_outcome(content, toml_merge_uninstall)
}

fn is_managed_entry(entry: &toml::Value) -> bool {
    entry.get(SENTINEL_KEY).and_then(|v| v.as_bool()) == Some(true)
}

fn managed_entry(event: &str, env_mode: bool, base_cmd: &str) -> toml::Value {
    let mut entry = Table::new();
    entry.insert("event".into(), toml::Value::String(event.into()));
    // env-mode events bake `--event <name>` (the shim reads DEEPSEEK_* env); the
    // subagent observer events forward the raw stdin JSON, so no `--event`.
    let command = if env_mode {
        format!("{base_cmd}{}{event}", crate::install::hook_cmd::EVENT_FLAG)
    } else {
        base_cmd.to_string()
    };
    entry.insert("command".into(), toml::Value::String(command));
    entry.insert(SENTINEL_KEY.into(), toml::Value::Boolean(true));
    toml::Value::Table(entry)
}

/// Install-schema verification: every CODEWHALE_EVENTS event still has a
/// sentinel-tagged `{event, command}` entry, AND `[hooks].enabled != false` — it
/// gates ALL hooks, so entries present under `enabled = false` is a silent-dead
/// the other checks miss.
pub(crate) fn verify_schema(content: &str) -> crate::install::verify::SchemaParse {
    use crate::install::verify::{SchemaParse, ShimRef, assemble, shell_shim_ref};
    let Ok(doc) = toml::from_str::<toml::Value>(content) else {
        return SchemaParse::broken("config.toml no longer parses as TOML");
    };
    let hooks = doc.get("hooks").and_then(|h| h.as_table());
    let entries: Vec<&toml::Value> = hooks
        .and_then(|h| h.get("hooks"))
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter(|e| is_managed_entry(e)).collect())
        .unwrap_or_default();
    let mut missing = Vec::new();
    let mut shim = ShimRef::Unknown;
    for &(ev, _) in CODEWHALE_EVENTS {
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
            None => missing.push(ev),
        }
    }
    let mut extra = Vec::new();
    if hooks
        .and_then(|h| h.get("enabled"))
        .and_then(|v| v.as_bool())
        == Some(false)
    {
        extra.push(
            "[hooks].enabled = false — CodeWhale gates ALL hooks on it, so none fire".to_string(),
        );
    }
    assemble(&missing, !entries.is_empty(), shim, extra)
}

fn toml_merge_install(doc: toml::Value, base_cmd: &str) -> toml::Value {
    let mut root = doc.as_table().cloned().unwrap_or_default();
    let hooks = root
        .entry("hooks".to_string())
        .or_insert_with(|| toml::Value::Table(Table::new()));
    if !hooks.is_table() {
        *hooks = toml::Value::Table(Table::new());
    }
    if let Some(hooks) = hooks.as_table_mut() {
        // Set the gate ONLY when the user made no explicit choice: an explicit
        // `enabled = false` is their own "all CodeWhale hooks off" switch, and we
        // could not faithfully restore it on disconnect. Ours then won't fire, but
        // `verify_schema` surfaces exactly that, so it isn't a silent no-sprite.
        hooks
            .entry("enabled".to_string())
            .or_insert_with(|| toml::Value::Boolean(true));
        let arr = hooks
            .entry("hooks".to_string())
            .or_insert_with(|| toml::Value::Array(vec![]));
        if !arr.is_array() {
            *arr = toml::Value::Array(vec![]);
        }
        if let Some(arr) = arr.as_array_mut() {
            arr.retain(|e| !is_managed_entry(e));
            for (ev, env_mode) in CODEWHALE_EVENTS {
                arr.push(managed_entry(ev, *env_mode, base_cmd));
            }
        }
    }
    toml::Value::Table(root)
}

fn toml_merge_uninstall(mut doc: toml::Value) -> toml::Value {
    let Some(root) = doc.as_table_mut() else {
        return doc;
    };
    let Some(toml::Value::Table(hooks)) = root.get_mut("hooks") else {
        return doc;
    };
    if let Some(arr) = hooks.get_mut("hooks").and_then(|h| h.as_array_mut()) {
        arr.retain(|e| !is_managed_entry(e));
    }
    if hooks
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|a| a.is_empty())
    {
        hooks.remove("hooks");
    }
    // A [hooks] table that is empty, or holds only the `enabled = true` a fresh
    // install writes, was ours — drop it. Install never writes `enabled = false`,
    // so a surviving one is the user's OWN switch and keeps its table alive.
    let ours_only = hooks.is_empty()
        || (hooks.keys().all(|k| k == "enabled")
            && hooks.get("enabled").and_then(|v| v.as_bool()) != Some(false));
    if ours_only {
        root.remove("hooks");
    }
    doc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> toml::Value {
        toml::from_str(s).unwrap()
    }

    const BASE: &str = "PIXTUOID_SOURCE=codewhale '/opt/bin/pixtuoid-hook'";

    #[test]
    fn env_paths_are_read_as_codewhale_reads_them() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".deepseek")).unwrap();
        let cw = home.path().join("cw");
        let padded = format!("  {}  ", cw.display());
        temp_env::with_vars(
            [
                ("CODEWHALE_CONFIG_PATH", None),
                ("DEEPSEEK_CONFIG_PATH", None),
                ("CODEWHALE_HOME", Some(padded.as_str())),
                ("HOME", Some(home.path().to_str().unwrap())),
            ],
            || {
                assert_eq!(
                    default_config_path().unwrap(),
                    cw.join("config.toml"),
                    "trimmed"
                );
                assert!(
                    !detect_installed(),
                    "an explicit CODEWHALE_HOME is not found by its legacy sibling"
                );
            },
        );
    }

    #[test]
    fn an_override_expands_its_tilde_and_must_be_absolute() {
        let home = tempfile::tempdir().unwrap();
        let home_str = home.path().to_str().unwrap();
        temp_env::with_vars(
            [
                ("CODEWHALE_CONFIG_PATH", None),
                ("DEEPSEEK_CONFIG_PATH", None),
                ("CODEWHALE_HOME", Some("~/cw")),
                ("HOME", Some(home_str)),
            ],
            || {
                assert_eq!(
                    default_config_path().unwrap(),
                    home.path().join("cw").join("config.toml")
                );
            },
        );
        for var in [
            "CODEWHALE_HOME",
            "CODEWHALE_CONFIG_PATH",
            "DEEPSEEK_CONFIG_PATH",
        ] {
            let mut vars = vec![
                ("CODEWHALE_CONFIG_PATH", None),
                ("DEEPSEEK_CONFIG_PATH", None),
                ("CODEWHALE_HOME", None),
                ("HOME", Some(home_str)),
            ];
            vars.retain(|(k, _)| *k != var);
            vars.push((var, Some("rel/cw")));
            temp_env::with_vars(vars, || {
                let err = default_config_path().unwrap_err().to_string();
                assert!(
                    err.contains(var) && err.contains("absolute"),
                    "{var}: {err}"
                );
            });
        }
    }

    #[test]
    fn a_set_home_decides_alone_and_must_be_absolute() {
        let profile = tempfile::tempdir().unwrap();
        temp_env::with_vars(
            [
                ("CODEWHALE_CONFIG_PATH", None),
                ("DEEPSEEK_CONFIG_PATH", None),
                ("CODEWHALE_HOME", None),
                ("HOME", Some("rel/home")),
                ("USERPROFILE", Some(profile.path().to_str().unwrap())),
            ],
            || {
                assert!(
                    cw_home().is_none(),
                    "a relative HOME is no home, not a fallback"
                );
                assert!(default_config_path().is_err());
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_override_is_not_tilde_expanded() {
        use std::os::unix::ffi::OsStrExt;
        let home = tempfile::tempdir().unwrap();
        let raw = std::ffi::OsStr::from_bytes(b"~/cw\xff");
        temp_env::with_vars(
            [
                ("CODEWHALE_CONFIG_PATH", None),
                ("DEEPSEEK_CONFIG_PATH", None),
                ("CODEWHALE_HOME", Some(raw)),
                ("HOME", Some(home.path().as_os_str())),
            ],
            || {
                assert!(
                    default_config_path().is_err(),
                    "verbatim, so relative, so refused"
                )
            },
        );
    }

    #[test]
    fn config_path_honors_codewhale_then_deepseek_env_overrides() {
        let p = resolve_config_path(
            Some("/custom/cw.toml".into()),
            Some("/custom/ds.toml".into()),
            Some("/ignored/home".into()),
            Some(PathBuf::from("/home/u")),
            |_| panic!("exists() must not be consulted when an env override is set"),
        )
        .unwrap();
        assert_eq!(p, PathBuf::from("/custom/cw.toml"));
        let p = resolve_config_path(None, Some("/custom/ds.toml".into()), None, None, |_| {
            panic!("exists() must not be consulted when an env override is set")
        })
        .unwrap();
        assert_eq!(p, PathBuf::from("/custom/ds.toml"));
    }

    #[test]
    fn config_path_codewhale_home_is_the_app_dir_verbatim() {
        let cw_home = "/custom/cwhome";
        let os_home = PathBuf::from("/home/u");
        let modern = PathBuf::from(cw_home).join("config.toml");
        let legacy = os_home.join(".deepseek").join("config.toml");
        let p = resolve_config_path(
            None,
            None,
            Some(cw_home.into()),
            Some(os_home.clone()),
            |q| q == modern,
        )
        .unwrap();
        assert_eq!(p, modern);
        let p = resolve_config_path(
            None,
            None,
            Some(cw_home.into()),
            Some(os_home.clone()),
            |q| q == legacy,
        )
        .unwrap();
        assert_eq!(
            p, modern,
            "an explicit CODEWHALE_HOME never falls back to .deepseek"
        );
        let p = resolve_config_path(None, None, Some(cw_home.into()), None, |_| false).unwrap();
        assert_eq!(p, modern);
    }

    #[test]
    fn config_path_prefers_modern_then_legacy_then_modern_for_fresh() {
        let home = PathBuf::from("/home/u");
        let modern = home.join(".codewhale").join("config.toml");
        let legacy = home.join(".deepseek").join("config.toml");
        let p = resolve_config_path(None, None, None, Some(home.clone()), |q| q == modern).unwrap();
        assert_eq!(p, modern);
        let p = resolve_config_path(None, None, None, Some(home.clone()), |q| q == legacy).unwrap();
        assert_eq!(p, legacy);
        let p = resolve_config_path(None, None, None, Some(home), |_| false).unwrap();
        assert_eq!(p, modern);
    }

    #[test]
    fn config_path_errors_when_no_home_and_no_override() {
        let err = resolve_config_path(None, None, None, None, |_| false).unwrap_err();
        assert!(
            err.to_string().ends_with("unset); set one and reconnect"),
            "no home + no override must surface the actionable error: {err}"
        );
    }

    #[test]
    fn install_creates_one_entry_per_event_with_baked_event_and_sentinel() {
        let out = merge_install("", BASE).unwrap();
        assert!(out.changed);
        let v = parse(&out.content);
        assert_eq!(
            v["hooks"]["enabled"].as_bool(),
            Some(true),
            "enabled must round-trip as a [hooks]-level scalar, not absorbed into an entry"
        );
        let arr = v["hooks"]["hooks"].as_array().unwrap();
        assert_eq!(arr.len(), CODEWHALE_EVENTS.len());
        for (entry, (ev, env_mode)) in arr.iter().zip(CODEWHALE_EVENTS) {
            assert_eq!(entry["event"].as_str().unwrap(), *ev);
            let expected = if *env_mode {
                format!("{BASE} --event {ev}")
            } else {
                BASE.to_string()
            };
            assert_eq!(
                entry["command"].as_str().unwrap(),
                expected,
                "env-mode events bake --event; subagent events use the plain stdin-forward command"
            );
            assert!(entry[SENTINEL_KEY].as_bool().unwrap());
        }
    }

    #[test]
    fn install_is_idempotent_and_replaces_across_paths() {
        let a = merge_install("", BASE).unwrap();
        let b = merge_install(&a.content, BASE).unwrap();
        assert!(!b.changed, "same-command re-install is a semantic no-op");
        let c = merge_install(
            &a.content,
            "PIXTUOID_SOURCE=codewhale '/usr/local/bin/pixtuoid-hook'",
        )
        .unwrap();
        let v = parse(&c.content);
        assert_eq!(
            v["hooks"]["hooks"].as_array().unwrap().len(),
            CODEWHALE_EVENTS.len(),
            "path change must not duplicate entries"
        );
    }

    #[test]
    fn install_respects_an_explicit_enabled_false_but_defaults_a_fresh_install_to_true() {
        let disabled = merge_install("[hooks]\nenabled = false\n", BASE).unwrap();
        assert_eq!(
            parse(&disabled.content)["hooks"]["enabled"].as_bool(),
            Some(false),
            "an explicit enabled = false is left untouched"
        );
        let fresh = merge_install("", BASE).unwrap();
        assert_eq!(
            parse(&fresh.content)["hooks"]["enabled"].as_bool(),
            Some(true),
            "a fresh install (no enabled key) defaults enabled = true"
        );
    }

    #[test]
    fn install_preserves_user_hooks_and_other_keys() {
        let user = r#"
provider = "deepseek"
api_key = "secret"

[hooks]
enabled = true

[[hooks.hooks]]
event = "session_start"
command = "echo hi"
"#;
        let out = merge_install(user, BASE).unwrap();
        let v = parse(&out.content);
        assert_eq!(v["provider"].as_str(), Some("deepseek"));
        assert_eq!(
            v["api_key"].as_str(),
            Some("secret"),
            "unrelated keys survive"
        );
        let arr = v["hooks"]["hooks"].as_array().unwrap();
        assert_eq!(arr.len(), 1 + CODEWHALE_EVENTS.len());
        assert!(
            arr.iter().any(|e| e["command"].as_str() == Some("echo hi")),
            "the user's own hook must be preserved"
        );
    }

    #[test]
    fn connect_respects_an_explicit_enabled_false_and_disconnect_preserves_it() {
        let user = "[hooks]\nenabled = false\n\n[[hooks.hooks]]\nevent = \"session_start\"\ncommand = \"my-own-hook\"\n";
        let installed = merge_install(user, BASE).unwrap();
        let v = parse(&installed.content);
        assert_eq!(
            v["hooks"]["enabled"].as_bool(),
            Some(false),
            "an explicit enabled = false is the user's global switch — leave it untouched on connect"
        );
        assert!(
            v["hooks"]["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .any(is_managed_entry),
            "our managed entries are still installed (they just won't fire until the user enables hooks)"
        );

        let removed = merge_uninstall(&installed.content).unwrap();
        let v = parse(&removed.content);
        assert_eq!(
            v["hooks"]["enabled"].as_bool(),
            Some(false),
            "disconnect must not remove the user's own enabled = false"
        );
        let arr = v["hooks"]["hooks"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "only the user's own hook remains");
        assert_eq!(arr[0]["command"].as_str(), Some("my-own-hook"));
        assert!(
            !arr.iter().any(is_managed_entry),
            "none of our managed entries survive disconnect"
        );
    }

    #[test]
    fn uninstall_removes_only_managed_entries() {
        let user = r#"
[hooks]
enabled = true

[[hooks.hooks]]
event = "session_start"
command = "echo hi"
"#;
        let installed = merge_install(user, BASE).unwrap();
        let cleaned = merge_uninstall(&installed.content).unwrap();
        assert!(cleaned.changed);
        let v = parse(&cleaned.content);
        let arr = v["hooks"]["hooks"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "only the user's own hook remains");
        assert_eq!(arr[0]["command"].as_str(), Some("echo hi"));
    }

    #[test]
    fn uninstall_of_pixtuoid_only_install_drops_the_hooks_table() {
        let installed = merge_install("", BASE).unwrap();
        let cleaned = merge_uninstall(&installed.content).unwrap();
        let v = parse(&cleaned.content);
        assert!(
            v.get("hooks").is_none(),
            "a pixtuoid-only [hooks] (just enabled + our entries) must be fully removed, got {v}"
        );
    }

    #[test]
    fn uninstall_no_managed_hooks_is_a_no_op() {
        let user = "[hooks]\nenabled = true\n\n[[hooks.hooks]]\nevent = \"session_start\"\ncommand = \"echo hi\"\n";
        let out = merge_uninstall(user).unwrap();
        assert!(!out.changed, "no managed entries → semantic no-op");
    }

    #[test]
    fn merge_install_rejects_invalid_toml() {
        assert!(merge_install("not = valid = toml", BASE).is_err());
    }

    #[test]
    fn install_coerces_non_table_hooks_and_non_array_entries() {
        let out = merge_install("hooks = \"garbage\"", BASE).unwrap();
        let v = parse(&out.content);
        assert!(v["hooks"].is_table());
        assert_eq!(
            v["hooks"]["hooks"].as_array().unwrap().len(),
            CODEWHALE_EVENTS.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn hook_command_is_the_base_env_prefix_form_without_event() {
        let cmd = hook_command(Path::new("/opt/bin/pixtuoid-hook"), false).unwrap();
        assert_eq!(cmd, "PIXTUOID_SOURCE=codewhale '/opt/bin/pixtuoid-hook'");
        assert!(
            !cmd.contains("--event"),
            "the event is appended by merge_install"
        );
    }

    #[test]
    #[cfg(windows)]
    fn hook_command_emits_bare_exec_form_with_source_flag_on_windows() {
        let cmd = hook_command(Path::new(r"C:\tools\pixtuoid-hook.exe"), false).unwrap();
        assert_eq!(cmd, r"C:\tools\pixtuoid-hook.exe --source codewhale");
    }

    #[test]
    #[cfg(unix)]
    fn hook_command_errors_on_non_utf8_path() {
        use std::os::unix::ffi::OsStrExt;
        let bad = Path::new(std::ffi::OsStr::from_bytes(b"/x/\xff/pixtuoid-hook"));
        assert!(hook_command(bad, false).is_err());
    }

    #[test]
    fn verify_schema_reports_broken_on_unparseable_toml() {
        use crate::install::verify::ShimRef;
        let res = verify_schema("not = = toml");
        assert_eq!(res.shim, ShimRef::Unknown);
        assert!(
            res.issues
                .iter()
                .any(|i| i.contains("no longer parses as TOML")),
            "unparseable TOML must surface the parse-broken issue, got {:?}",
            res.issues
        );
    }

    #[test]
    fn verify_schema_flags_partial_install_missing_events() {
        use crate::install::verify::ShimRef;
        let cfg = "[hooks]\nenabled = true\n\n[[hooks.hooks]]\nevent = \"session_start\"\ncommand = \"x\"\n_pixtuoid = true\n";
        let res = verify_schema(cfg);
        let joined = res.issues.join(" | ");
        assert!(
            joined.contains("missing hook entries for"),
            "a partial install must report missing events, got {:?}",
            res.issues
        );
        assert!(
            joined.contains("tool_call_before"),
            "tool_call_before is a registered-but-absent event and must be listed, got {:?}",
            res.issues
        );
        assert!(
            !joined.contains("_pixtuoid` sentinel is absent"),
            "a present sentinel entry must NOT trip the no-managed-entries issue"
        );
        assert_ne!(res.shim, ShimRef::Unknown);
    }

    #[test]
    fn verify_schema_flags_enabled_false_and_passes_full_install() {
        use crate::install::verify::ShimRef;
        let full = merge_install("", BASE).unwrap().content;
        let sound = verify_schema(&full);
        assert!(
            sound.issues.is_empty(),
            "a full install must be issue-free, got {:?}",
            sound.issues
        );
        assert_ne!(
            sound.shim,
            ShimRef::Unknown,
            "a full install must resolve a shim ref from its managed commands"
        );

        let disabled = full.replacen("enabled = true", "enabled = false", 1);
        assert!(
            disabled.contains("enabled = false"),
            "the test fixture must actually flip the flag"
        );
        let res = verify_schema(&disabled);
        let joined = res.issues.join(" | ");
        assert!(
            joined.contains("enabled = false"),
            "enabled=false must be flagged, got {:?}",
            res.issues
        );
        assert!(
            joined.contains("none fire"),
            "the enabled=false issue must explain that no hooks fire, got {:?}",
            res.issues
        );
        assert!(
            !joined.contains("missing hook entries for"),
            "a complete-but-disabled install reports the gate, not missing events"
        );
    }

    #[test]
    fn install_coerces_inner_non_array_hooks_key() {
        let out = merge_install("[hooks]\nhooks = \"garbage\"\n", BASE).unwrap();
        let v = parse(&out.content);
        assert!(v["hooks"].is_table());
        assert!(
            v["hooks"]["hooks"].is_array(),
            "the scalar `hooks` key must be coerced to an array"
        );
        assert_eq!(
            v["hooks"]["hooks"].as_array().unwrap().len(),
            CODEWHALE_EVENTS.len(),
            "after coercion every managed event must be written"
        );
    }

    #[test]
    fn every_registered_codewhale_event_decodes() {
        use pixtuoid_core::source::decoder::decode_hook_payload;
        for (ev, _env_mode) in CODEWHALE_EVENTS {
            // Carry every identity field a decoder arm might need: `cwd` for the
            // env-mode events, `agent_id`/`workspace` for the subagent events.
            let payload = serde_json::json!({
                "event": ev,
                "cwd": "/repo",
                "agent_id": "agent-1",
                "workspace": "/repo",
                "_pixtuoid_source": "codewhale",
            });
            assert!(
                decode_hook_payload(payload).is_ok(),
                "registered CodeWhale hook {ev:?} has no decoder arm — it would \
                 bail as unsupported. Add an arm in pixtuoid-core source/codewhale.rs."
            );
        }
    }

    #[test]
    fn codewhale_events_pins_the_exact_registered_set() {
        // The pinned bool is `env_mode`: flipping one silently changes whether
        // `--event <name>` is baked into the installed command.
        crate::install::assert_event_roster(
            "CODEWHALE_EVENTS",
            CODEWHALE_EVENTS,
            &[
                ("session_start", true),
                ("message_submit", true),
                ("tool_call_before", true),
                ("tool_call_after", true),
                ("session_end", true),
                ("subagent_spawn", false),
                ("subagent_complete", false),
            ],
        );
    }
}
