//! Jcode's directly executed commands in its shared TOML config.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use toml::{Table, Value};

use super::hook_cmd::{SOURCE_FLAG, shell_single_quote};
use super::merge::{hook_path_str, parse_toml_or_empty, toml_merge_outcome, with_toml_table};
use super::target::MergeOutcome;
use super::verify::{SchemaParse, ShimRef, assemble, posix_unquote};
use pixtuoid_core::source::jcode::SOURCE_NAME;

/// No `session_start`: each client connection fires one for a provisional
/// session a resume throws away (`crates/jcode-app-core/src/agent.rs:648`,
/// `:659` at v0.99.1), so a session registers on its first `turn_start`.
pub(crate) const JCODE_EVENTS: &[&str] = &[
    "session_end",
    "turn_start",
    "turn_end",
    "pre_tool",
    "post_tool",
];

pub(crate) fn default_config_path() -> Result<PathBuf> {
    jcode_home()
        .map(|home| home.join("config.toml"))
        .ok_or_else(|| super::io::home_unset("Jcode's home", "JCODE_HOME"))
}

fn jcode_home() -> Option<PathBuf> {
    pixtuoid_core::platform::path_env("JCODE_HOME")
        // jcode-storage::jcode_dir reads it with `env::var`.
        .filter(|home| home.to_str().is_some())
        .or_else(|| native_home().map(|home| home.join(".jcode")))
}

/// `dirs::home_dir`, Jcode's resolver, which no `USERPROFILE` moves on Windows.
fn native_home() -> Option<PathBuf> {
    #[cfg(windows)]
    return pixtuoid_core::platform::profile_dir();
    #[cfg(not(windows))]
    std::env::home_dir()
}

pub(crate) fn detect_installed() -> bool {
    jcode_home().is_some_and(|home| home.is_dir())
}

// Jcode's jcode-terminal-launch::parse_hook_command is POSIX-like on every OS,
// but executes directly: an env prefix would become the executable name.
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    ensure!(
        resolved.is_absolute(),
        "Jcode requires an absolute hook binary path"
    );
    let path = shell_single_quote(hook_path_str(resolved)?);
    Ok(format!("{path}{SOURCE_FLAG}{SOURCE_NAME}"))
}

fn managed_path(command: &str) -> Option<PathBuf> {
    let quoted = command.strip_suffix(&format!("{SOURCE_FLAG}{SOURCE_NAME}"))?;
    let path = PathBuf::from(posix_unquote(quoted));
    // Round-trip equality excludes prefixes, suffixes and shell fragments without
    // narrowing explicit shim overrides to one basename.
    (hook_command(&path, true).ok().as_deref() == Some(command)).then_some(path)
}

/// Refuses what a merge would coerce: [`with_toml_table`] turns `hooks = 1` into
/// an empty table.
fn refuse_foreign_shape(content: &str) -> Result<()> {
    let root = parse_toml_or_empty(content)?;
    let Some(hooks) = root.get("hooks") else {
        return Ok(());
    };
    let hooks = hooks
        .as_table()
        .context("`hooks` is not a table — refusing to overwrite")?;
    for event in JCODE_EVENTS {
        let valid = match hooks.get(*event) {
            None | Some(Value::String(_)) => true,
            Some(Value::Array(commands)) => commands.iter().all(Value::is_str),
            Some(_) => false,
        };
        ensure!(
            valid,
            "`hooks.{event}` is neither a command nor an array of them — refusing to overwrite"
        );
    }
    Ok(())
}

/// Puts `ours` in place of each event's managed commands, or removes them,
/// keeping every other command.
fn retarget(hooks: &mut Table, ours: Option<&str>) {
    let managed = |command: &Value| command.as_str().is_some_and(|c| managed_path(c).is_some());
    for event in JCODE_EVENTS {
        let merged = match (hooks.remove(*event), ours) {
            (None, ours) => ours.map(Value::from),
            (Some(Value::Array(mut commands)), ours) => {
                commands.retain(|c| !managed(c));
                commands.extend(ours.map(Value::from));
                Some(Value::Array(commands))
            }
            (Some(command), ours) if managed(&command) => ours.map(Value::from),
            (Some(foreign), Some(ours)) => Some(Value::Array(vec![foreign, ours.into()])),
            (foreign, None) => foreign,
        };
        if let Some(merged) = merged {
            hooks.insert((*event).to_owned(), merged);
        }
    }
}

pub(crate) fn merge_install(content: &str, command: &str) -> Result<MergeOutcome> {
    refuse_foreign_shape(content)?;
    toml_merge_outcome(content, |mut root| {
        with_toml_table(&mut root, "hooks", |hooks| retarget(hooks, Some(command)));
        root
    })
}

pub(crate) fn merge_uninstall(content: &str) -> Result<MergeOutcome> {
    toml_merge_outcome(content, |mut root| {
        // An emptied `[hooks]` stays: the text above a header goes with its table,
        // and Jcode's default config keeps a section's commented options there.
        if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_table_mut) {
            retarget(hooks, None);
        }
        root
    })
}

pub(crate) fn verify_schema(content: &str) -> SchemaParse {
    let Ok(doc) = toml::from_str::<Table>(content) else {
        return SchemaParse::broken("config.toml no longer parses as TOML");
    };
    let hooks = doc.get("hooks").and_then(Value::as_table);
    let mut missing = Vec::new();
    let mut shim = ShimRef::Unknown;
    for event in JCODE_EVENTS {
        let managed = match hooks.and_then(|hooks| hooks.get(*event)) {
            Some(Value::Array(commands)) => commands.as_slice(),
            Some(command) => std::slice::from_ref(command),
            None => &[],
        }
        .iter()
        .find_map(|command| command.as_str().and_then(managed_path));
        match managed {
            Some(path) if shim == ShimRef::Unknown => shim = ShimRef::Absolute(path),
            Some(_) => {}
            None => missing.push(*event),
        }
    }
    // The shared `assemble` no-managed message names a sentinel a command can't
    // carry here.
    if shim == ShimRef::Unknown {
        return SchemaParse::broken(format!(
            "no managed pixtuoid hook entries in [hooks] (no `{SOURCE_FLAG}{SOURCE_NAME}` command \
             — the config was hand-edited or hooks were never installed)"
        ));
    }
    assemble(&missing, true, shim, vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> String {
        hook_command(
            &std::env::temp_dir().join("a dir/O'Brien/pixtuoid-hook"),
            true,
        )
        .unwrap()
    }

    #[test]
    fn every_registered_jcode_event_decodes() {
        for event in JCODE_EVENTS {
            let payload = serde_json::json!({
                "event": event,
                "session_id": "session_chipmunk_123_random",
                "tool_name": "bash",
                "_pixtuoid_source": SOURCE_NAME,
            });
            assert!(
                pixtuoid_core::source::decoder::decode_hook_payload(payload).is_ok(),
                "{event}"
            );
        }
    }

    #[test]
    fn jcode_events_pins_the_exact_registered_set() {
        crate::install::assert_event_roster(
            "JCODE_EVENTS",
            JCODE_EVENTS,
            &[
                "session_end",
                "turn_start",
                "turn_end",
                "pre_tool",
                "post_tool",
            ],
        );
    }

    #[test]
    fn jcode_config_path_uses_the_shared_home_and_reads_an_empty_one_as_unset() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("JCODE_HOME", Some(home.path()), || {
            assert_eq!(
                default_config_path().unwrap(),
                home.path().join("config.toml")
            );
            assert!(detect_installed());
        });
        #[cfg(unix)]
        for unset in [None, Some("".as_ref())] {
            temp_env::with_vars(
                [
                    ("JCODE_HOME", unset),
                    ("HOME", Some(home.path().as_os_str())),
                ],
                || {
                    assert_eq!(jcode_home(), Some(home.path().join(".jcode")));
                },
            );
        }
    }

    #[test]
    fn jcode_inline_arrays_deduplicate_without_removing_foreign_commands() {
        let command = command();
        let input = format!(
            "hooks = {{ pre_tool = ['foreign', {0}, {0}] }}\r\n",
            Value::String(command.clone())
        );
        let installed = merge_install(&input, &command).unwrap();
        assert!(!installed.content.replace("\r\n", "").contains('\n'));
        let doc: Value = toml::from_str(&installed.content).unwrap();
        assert_eq!(doc["hooks"]["pre_tool"].as_array().unwrap().len(), 2);
        let removed = merge_uninstall(&installed.content).unwrap();
        let doc: Value = toml::from_str(&removed.content).unwrap();
        assert_eq!(doc["hooks"]["pre_tool"].as_array().unwrap().len(), 1);
        assert_eq!(doc["hooks"]["pre_tool"][0].as_str(), Some("foreign"));
    }

    #[test]
    fn jcode_malformed_config_never_rewrites_on_connect_or_disconnect() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        for (malformed, parses) in [
            ("[hooks", false),
            ("hooks = 1", true),
            ("[hooks]\npre_tool = 3", true),
            ("[hooks]\nturn_end = ['ok', 7]", true),
        ] {
            std::fs::write(&config, malformed).unwrap();
            let error = super::super::install_target_from(
                &super::super::target::JCODE,
                Some(config.clone()),
                Some(std::env::current_exe().unwrap()),
            )
            .unwrap_err();
            assert!(format!("{error:#}").contains("refusing to overwrite"));
            assert_eq!(std::fs::read_to_string(&config).unwrap(), malformed);
            let removed =
                super::super::uninstall_target(&super::super::target::JCODE, Some(config.clone()));
            assert_eq!(removed.is_ok(), parses);
            assert_eq!(std::fs::read_to_string(&config).unwrap(), malformed);
        }
    }

    #[cfg(unix)]
    #[test]
    fn jcode_connect_disconnect_writes_through_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("dotfiles.toml");
        let link = temp.path().join("config.toml");
        std::fs::write(&real, "[hooks]\npre_tool = 'keep' # gate\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let target = &super::super::target::JCODE;
        super::super::install_target_from(
            target,
            Some(link.clone()),
            Some(std::env::current_exe().unwrap()),
        )
        .unwrap();
        super::super::uninstall_target(target, Some(link.clone())).unwrap();
        assert!(link.is_symlink());
        assert!(std::fs::read_to_string(real).unwrap().contains("keep"));
    }

    #[test]
    fn jcode_disconnect_keeps_the_section_above_the_hooks_table_it_emptied() {
        let original = "[notifications]\nenabled = true\n# turn_complete = true\n\n[hooks]\n";
        let installed = merge_install(original, &command()).unwrap();
        let removed = merge_uninstall(&installed.content).unwrap();
        assert!(removed.changed);
        assert_eq!(removed.content, original);
        assert!(!merge_uninstall(&removed.content).unwrap().changed);
    }

    #[test]
    fn jcode_keeps_foreign_commands_and_timeouts() {
        let original = "[hooks]\nturn_start = 'notify me'\npre_tool = [\n  'gate one',\n  'gate two',\n]\npre_tool_timeout_ms = 1234\npost_tool_feedback = 'review'\n\n[other]\nvalue = 7\n";
        let cmd = command();
        let installed = merge_install(original, &cmd).unwrap();
        assert!(installed.changed);
        let doc: Value = toml::from_str(&installed.content).unwrap();
        assert_eq!(doc["hooks"]["turn_start"][0].as_str(), Some("notify me"));
        assert_eq!(doc["hooks"]["pre_tool"][0].as_str(), Some("gate one"));
        assert_eq!(doc["hooks"]["pre_tool"][1].as_str(), Some("gate two"));
        assert_eq!(doc["hooks"]["pre_tool_timeout_ms"].as_integer(), Some(1234));
        assert!(verify_schema(&installed.content).issues.is_empty());
        let again = merge_install(&installed.content, &cmd).unwrap();
        assert!(!again.changed);
        assert_eq!(again.content, installed.content);
        let removed = merge_uninstall(&installed.content).unwrap();
        let doc: Value = toml::from_str(&removed.content).unwrap();
        assert_eq!(doc["hooks"]["pre_tool"].as_array().unwrap().len(), 2);
        assert_eq!(doc["hooks"]["post_tool_feedback"].as_str(), Some("review"));
        assert!(!merge_uninstall(&removed.content).unwrap().changed);
    }

    #[test]
    fn jcode_owns_only_exact_direct_commands() {
        let cmd = command();
        assert!(managed_path(&cmd).is_some());
        for other in [
            format!("echo {cmd}"),
            format!("{cmd} --extra"),
            cmd.replace("jcode", "jcode-other"),
            "'relative/pixtuoid-hook' --source jcode".into(),
            "PIXTUOID_SOURCE=jcode '/bin/pixtuoid-hook'".into(),
        ] {
            assert!(managed_path(&other).is_none(), "{other}");
            let input = format!("[hooks]\nturn_start = {}\n", Value::String(other));
            assert!(!merge_uninstall(&input).unwrap().changed);
        }
        assert!(!cmd.contains("PIXTUOID_SOURCE="));
        assert!(hook_command(std::path::Path::new("relative"), false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn jcode_quotes_apostrophes_and_ignores_non_unicode_home_override() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            hook_command(Path::new("/a'b c/pixtuoid-hook"), false).unwrap(),
            "'/a'\\''b c/pixtuoid-hook' --source jcode"
        );
        let invalid = std::ffi::OsStr::from_bytes(b"/invalid/\xff");
        assert!(hook_command(Path::new(invalid), true).is_err());
        let home = tempfile::tempdir().unwrap();
        temp_env::with_vars(
            [
                ("JCODE_HOME", Some(invalid)),
                ("HOME", Some(home.path().as_os_str())),
            ],
            || {
                assert_eq!(jcode_home(), Some(home.path().join(".jcode")));
            },
        );
    }

    #[cfg(windows)]
    #[test]
    fn jcode_windows_native_profile_ignores_spoofed_environment() {
        use std::os::windows::ffi::OsStringExt;
        let expected = native_home()
            .expect("Windows profile folder")
            .join(".jcode");
        let spoof = tempfile::tempdir().unwrap();
        temp_env::with_vars(
            [
                ("JCODE_HOME", None),
                ("HOME", Some(spoof.path().as_os_str())),
                ("USERPROFILE", Some(spoof.path().as_os_str())),
            ],
            || {
                assert_eq!(jcode_home(), Some(expected.clone()));
                let invalid = std::ffi::OsString::from_wide(&[0xd800]);
                temp_env::with_var("JCODE_HOME", Some(invalid), || {
                    assert_eq!(jcode_home(), Some(expected.clone()));
                });
            },
        );
    }

    #[cfg(windows)]
    #[test]
    fn jcode_windows_uses_posix_quoting_not_cmd_exe_rules() {
        assert_eq!(
            hook_command(Path::new(r"C:\O'Brien\pixtuoid-hook.exe"), false).unwrap(),
            r"'C:\O'\''Brien\pixtuoid-hook.exe' --source jcode"
        );
    }

    #[test]
    fn jcode_updates_owned_paths_and_verifies_every_event() {
        let old = merge_install("", &command()).unwrap();
        let new_path = std::env::temp_dir().join("new hook");
        let new = hook_command(&new_path, false).unwrap();
        let updated = merge_install(&old.content, &new).unwrap();
        let doc: Value = toml::from_str(&updated.content).unwrap();
        for event in JCODE_EVENTS {
            assert_eq!(doc["hooks"][event].as_str(), Some(new.as_str()));
        }
        assert_eq!(
            verify_schema(&updated.content).shim,
            crate::install::verify::ShimRef::Absolute(new_path)
        );
        for event in JCODE_EVENTS {
            let mut doc: Table = toml::from_str(&updated.content).unwrap();
            doc["hooks"].as_table_mut().unwrap().remove(*event);
            assert!(
                !verify_schema(&toml::to_string(&doc).unwrap())
                    .issues
                    .is_empty()
            );
        }
        assert!(!merge_uninstall("").unwrap().changed);
        let unmanaged = &verify_schema("").issues;
        assert!(unmanaged[0].contains("--source jcode"), "{unmanaged:?}");
    }
}
