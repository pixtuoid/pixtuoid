//! Jcode's directly executed commands in its shared TOML config.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use toml_edit::{Array, DocumentMut, Item, Table, Value};

use super::target::MergeOutcome;
use super::verify::{SchemaParse, ShimRef};
use pixtuoid_core::source::jcode::SOURCE_NAME;

pub(crate) const JCODE_EVENTS: &[&str] = &[
    "session_start",
    "session_end",
    "turn_start",
    "turn_end",
    "pre_tool",
    "post_tool",
];

const OWNERSHIP_FLAG: &str = " --pixtuoid-managed";

pub(crate) fn default_config_path() -> Result<PathBuf> {
    jcode_home()
        .map(|home| home.join("config.toml"))
        .ok_or_else(|| super::io::home_unset("the home directory", "HOME/USERPROFILE"))
}

fn jcode_home() -> Option<PathBuf> {
    // jcode-storage::jcode_dir accepts even an empty Unicode JCODE_HOME.
    std::env::var_os("JCODE_HOME")
        .and_then(|home| home.into_string().ok())
        .map(PathBuf::from)
        .or_else(|| native_home().map(|home| home.join(".jcode")))
}

#[cfg(not(windows))]
fn native_home() -> Option<PathBuf> {
    std::env::home_dir().filter(|home| !home.as_os_str().is_empty())
}

#[cfg(windows)]
fn native_home() -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Profile, SHGetKnownFolderPath};

    let mut raw = std::ptr::null_mut();
    // SAFETY: the API returns a terminated wide allocation, owned until CoTaskMemFree.
    // https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shgetknownfolderpath
    unsafe {
        let status = SHGetKnownFolderPath(&FOLDERID_Profile, 0, std::ptr::null_mut(), &mut raw);
        let home = if status >= 0 && !raw.is_null() {
            let mut len = 0;
            while *raw.add(len) != 0 {
                len += 1;
            }
            Some(PathBuf::from(OsString::from_wide(
                std::slice::from_raw_parts(raw, len),
            )))
        } else {
            None
        };
        CoTaskMemFree(raw.cast());
        home
    }
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
    let path = super::merge::hook_path_str(resolved)?;
    Ok(format!(
        "'{}'{}{SOURCE_NAME}{OWNERSHIP_FLAG}",
        path.replace('\'', "'\\''"),
        super::hook_cmd::SOURCE_FLAG
    ))
}

fn managed_path(command: &str) -> Option<PathBuf> {
    let quoted = command.strip_suffix(&format!(
        "{}{SOURCE_NAME}{OWNERSHIP_FLAG}",
        super::hook_cmd::SOURCE_FLAG
    ))?;
    if !quoted.starts_with('\'') || !quoted.ends_with('\'') {
        return None;
    }
    let path = PathBuf::from(super::verify::posix_unquote(quoted));
    // Round-trip equality excludes prefixes, suffixes and shell fragments without
    // narrowing explicit shim overrides to one basename.
    (hook_command(&path, true).ok().as_deref() == Some(command)).then_some(path)
}

fn parse(content: &str) -> Result<DocumentMut> {
    let doc: DocumentMut = content
        .parse()
        .context("refusing to overwrite malformed Jcode TOML")?;
    if let Some(hooks) = doc.get("hooks") {
        let hooks = hooks
            .as_table_like()
            .context("refusing to overwrite non-table hooks")?;
        for event in JCODE_EVENTS {
            if let Some(item) = hooks.get(event) {
                let valid = item.as_str().is_some()
                    || item
                        .as_array()
                        .is_some_and(|array| array.iter().all(|v| v.as_str().is_some()));
                ensure!(
                    valid,
                    "refusing to overwrite hooks.{event}: expected a command string or string array"
                );
            }
        }
    }
    Ok(doc)
}

pub(crate) fn merge_install(content: &str, command: &str) -> Result<MergeOutcome> {
    ensure!(
        managed_path(command).is_some(),
        "invalid Jcode managed hook command"
    );
    merge(content, Some(command))
}

pub(crate) fn merge_uninstall(content: &str) -> Result<MergeOutcome> {
    merge(content, None)
}

fn merge(content: &str, command: Option<&str>) -> Result<MergeOutcome> {
    let mut doc = parse(content)?;
    if command.is_some() && !doc.contains_key("hooks") {
        doc.insert("hooks", Item::Table(Table::new()));
    }
    if let Some(hooks) = doc.get_mut("hooks").and_then(Item::as_table_like_mut) {
        for event in JCODE_EVENTS {
            match hooks.get_mut(event) {
                Some(item) if item.as_str().is_some() => {
                    let owned = item.as_str().and_then(managed_path).is_some();
                    match (owned, command) {
                        (true, None) => {
                            hooks.remove(event);
                        }
                        (true, Some(command)) => {
                            let decor = item.as_value().context("command value")?.decor().clone();
                            let mut value = Value::from(command);
                            *value.decor_mut() = decor;
                            *item = Item::Value(value);
                        }
                        (false, Some(command)) => {
                            let mut value = item.as_value().context("command value")?.clone();
                            let decor = value.decor().clone();
                            *value.decor_mut() = Default::default();
                            let mut array = Array::new();
                            array.push_formatted(value);
                            array.push(command);
                            let mut value = Value::Array(array);
                            *value.decor_mut() = decor;
                            *item = Item::Value(value);
                        }
                        (false, None) => {}
                    }
                }
                Some(item) => {
                    let array = item.as_array_mut().context("command array")?;
                    let owned: Vec<usize> = array
                        .iter()
                        .enumerate()
                        .filter_map(|(i, v)| v.as_str().and_then(managed_path).map(|_| i))
                        .collect();
                    if let (Some(command), [index]) = (command, owned.as_slice())
                        && array.get(*index).and_then(Value::as_str) == Some(command)
                    {
                        continue;
                    }
                    for index in owned.into_iter().rev() {
                        array.remove(index);
                    }
                    if let Some(command) = command {
                        array.push(command);
                    }
                }
                None => {
                    if let Some(command) = command {
                        hooks.insert(event, toml_edit::value(command));
                    }
                }
            }
        }
    }
    let mut output = doc.to_string();
    let before: toml::Value = toml::from_str(content)?;
    let after: toml::Value = toml::from_str(&output)?;
    let changed = before != after;
    if !changed {
        output = content.to_owned();
    } else if content.contains("\r\n") && !content.replace("\r\n", "").contains('\n') {
        output = output.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    Ok(MergeOutcome {
        content: output,
        changed,
    })
}

pub(crate) fn verify_schema(content: &str) -> SchemaParse {
    let doc = match parse(content) {
        Ok(doc) => doc,
        Err(error) => return SchemaParse::broken(error.to_string()),
    };
    let hooks = doc.get("hooks").and_then(Item::as_table_like);
    let mut result = SchemaParse::default();
    for event in JCODE_EVENTS {
        let item = hooks.and_then(|hooks| hooks.get(event));
        let path = item.and_then(|item| {
            item.as_str().and_then(managed_path).or_else(|| {
                item.as_array()?
                    .iter()
                    .filter_map(|v| v.as_str().and_then(managed_path))
                    .next()
            })
        });
        match path {
            Some(path) if result.shim == ShimRef::Unknown => result.shim = ShimRef::Absolute(path),
            Some(path) if !matches!(&result.shim, ShimRef::Absolute(installed) if installed == &path) => {
                result.issues.push(format!(
                    "Jcode hook {event} references a different shim; reconnect the source"
                ))
            }
            Some(_) => {}
            None => result
                .issues
                .push(format!("missing managed Jcode hook: {event}")),
        }
    }
    result
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
    fn jcode_config_path_uses_the_shared_home_even_when_empty() {
        let home = tempfile::tempdir().unwrap();
        temp_env::with_var("JCODE_HOME", Some(home.path()), || {
            assert_eq!(
                default_config_path().unwrap(),
                home.path().join("config.toml")
            );
            assert!(detect_installed());
        });
        temp_env::with_var("JCODE_HOME", Some(""), || {
            assert_eq!(default_config_path().unwrap(), PathBuf::from("config.toml"));
        });
        #[cfg(unix)]
        temp_env::with_vars(
            [
                ("JCODE_HOME", None),
                ("HOME", Some(home.path().as_os_str())),
            ],
            || {
                assert_eq!(jcode_home(), Some(home.path().join(".jcode")));
            },
        );
    }

    #[test]
    fn jcode_inline_arrays_deduplicate_without_removing_foreign_commands() {
        let command = command();
        let input = format!(
            "hooks = {{ pre_tool = ['foreign', {0}, {0}] }}\r\n",
            toml::Value::String(command.clone())
        );
        let installed = merge_install(&input, &command).unwrap();
        assert!(!installed.content.replace("\r\n", "").contains('\n'));
        let doc: toml::Value = toml::from_str(&installed.content).unwrap();
        assert_eq!(doc["hooks"]["pre_tool"].as_array().unwrap().len(), 2);
        let removed = merge_uninstall(&installed.content).unwrap();
        let doc: toml::Value = toml::from_str(&removed.content).unwrap();
        assert_eq!(doc["hooks"]["pre_tool"].as_array().unwrap().len(), 1);
        assert_eq!(doc["hooks"]["pre_tool"][0].as_str(), Some("foreign"));
    }

    #[test]
    fn jcode_malformed_config_never_rewrites_on_connect_or_disconnect() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        for malformed in ["[hooks", "hooks = 1", "[hooks]\npre_tool = [7]"] {
            std::fs::write(&config, malformed).unwrap();
            assert!(
                super::super::install_target_from(
                    &super::super::target::JCODE,
                    Some(config.clone()),
                    Some(std::env::current_exe().unwrap()),
                )
                .is_err()
            );
            assert_eq!(std::fs::read_to_string(&config).unwrap(), malformed);
            assert!(
                super::super::uninstall_target(&super::super::target::JCODE, Some(config.clone()))
                    .is_err()
            );
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
        assert!(super::super::has_hooks(target, Some(link.clone())));
        assert!(super::super::verify_target(target, Some(link.clone())).is_sound());
        super::super::uninstall_target(target, Some(link.clone())).unwrap();
        assert!(link.is_symlink());
        assert!(!super::super::has_hooks(target, Some(link)));
        let content = std::fs::read_to_string(real).unwrap();
        assert!(content.contains("keep"));
        assert!(content.contains("# gate"));
    }

    #[test]
    fn jcode_preserves_scalar_array_comments_and_timeouts() {
        let original = "# config\n[hooks] # hooks\nsession_start = 'notify me' # scalar\npre_tool = [\n  'gate one', # gate\n  'gate two',\n] # gates\npre_tool_timeout_ms = 1234 # timeout\npost_tool_feedback = 'review'\n\n[other]\nvalue = 7 # untouched\n";
        let cmd = command();
        let installed = merge_install(original, &cmd).unwrap();
        assert!(installed.changed);
        for comment in [
            "# config",
            "# hooks",
            "# scalar",
            "# gate",
            "# gates",
            "# timeout",
            "# untouched",
        ] {
            assert!(
                installed.content.contains(comment),
                "lost {comment}: {}",
                installed.content
            );
        }
        let doc: toml::Value = toml::from_str(&installed.content).unwrap();
        assert_eq!(doc["hooks"]["session_start"][0].as_str(), Some("notify me"));
        assert_eq!(doc["hooks"]["pre_tool"][0].as_str(), Some("gate one"));
        assert_eq!(doc["hooks"]["pre_tool"][1].as_str(), Some("gate two"));
        assert_eq!(doc["hooks"]["pre_tool_timeout_ms"].as_integer(), Some(1234));
        assert!(verify_schema(&installed.content).issues.is_empty());
        let again = merge_install(&installed.content, &cmd).unwrap();
        assert!(!again.changed);
        assert_eq!(again.content, installed.content);
        let removed = merge_uninstall(&installed.content).unwrap();
        for comment in [
            "# config",
            "# hooks",
            "# scalar",
            "# gate",
            "# gates",
            "# timeout",
            "# untouched",
        ] {
            assert!(
                removed.content.contains(comment),
                "lost {comment}: {}",
                removed.content
            );
        }
        let doc: toml::Value = toml::from_str(&removed.content).unwrap();
        assert_eq!(doc["hooks"]["pre_tool"].as_array().unwrap().len(), 2);
        assert_eq!(doc["hooks"]["post_tool_feedback"].as_str(), Some("review"));
        assert!(!merge_uninstall(&removed.content).unwrap().changed);
    }

    #[test]
    fn jcode_rejects_malformed_hook_shapes() {
        for input in [
            "[hooks",
            "hooks = 1",
            "[hooks]\npre_tool = 3",
            "[hooks]\nturn_end = ['ok', 7]",
        ] {
            assert!(merge_install(input, &command()).is_err(), "{input}");
            assert!(merge_uninstall(input).is_err(), "{input}");
        }
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
            let input = format!("[hooks]\nsession_start = {}\n", toml::Value::String(other));
            assert!(!merge_uninstall(&input).unwrap().changed);
        }
        assert!(!cmd.contains("PIXTUOID_SOURCE="));
        assert!(hook_command(std::path::Path::new("relative"), false).is_err());
    }

    #[test]
    fn jcode_preserves_foreign_source_flag_without_ownership_marker() {
        let foreign = "'/usr/local/bin/session-monitor' --source jcode";
        let original = format!(
            "[hooks]\nsession_start = {}\n",
            toml::Value::String(foreign.into())
        );
        assert!(!merge_uninstall(&original).unwrap().changed);
        let installed = merge_install(&original, &command()).unwrap();
        let doc: toml::Value = toml::from_str(&installed.content).unwrap();
        assert_eq!(doc["hooks"]["session_start"][0].as_str(), Some(foreign));
        let removed = merge_uninstall(&installed.content).unwrap();
        let doc: toml::Value = toml::from_str(&removed.content).unwrap();
        assert_eq!(doc["hooks"]["session_start"][0].as_str(), Some(foreign));
    }

    #[cfg(unix)]
    #[test]
    fn jcode_quotes_apostrophes_and_ignores_non_unicode_home_override() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            hook_command(Path::new("/a'b c/pixtuoid-hook"), false).unwrap(),
            "'/a'\\''b c/pixtuoid-hook' --source jcode --pixtuoid-managed"
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

    #[cfg(unix)]
    #[test]
    fn jcode_empty_home_uses_the_same_os_fallback_as_unset_home() {
        temp_env::with_vars_unset(["JCODE_HOME", "HOME"], || {
            let expected = std::env::home_dir()
                .filter(|home| !home.as_os_str().is_empty())
                .map(|home| home.join(".jcode"));
            assert_eq!(jcode_home(), expected);
            temp_env::with_var("HOME", Some(""), || assert_eq!(jcode_home(), expected));
        });
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
            r"'C:\O'\''Brien\pixtuoid-hook.exe' --source jcode --pixtuoid-managed"
        );
    }

    #[test]
    fn jcode_updates_owned_paths_and_verifies_every_event() {
        let old = merge_install("", &command()).unwrap();
        let new_path = std::env::temp_dir().join("new hook");
        let new = hook_command(&new_path, false).unwrap();
        let updated = merge_install(&old.content, &new).unwrap();
        let doc: toml::Value = toml::from_str(&updated.content).unwrap();
        for event in JCODE_EVENTS {
            assert_eq!(doc["hooks"][event].as_str(), Some(new.as_str()));
        }
        assert_eq!(
            verify_schema(&updated.content).shim,
            crate::install::verify::ShimRef::Absolute(new_path)
        );
        for event in JCODE_EVENTS {
            let mut doc = updated.content.parse::<toml_edit::DocumentMut>().unwrap();
            doc["hooks"].as_table_like_mut().unwrap().remove(event);
            assert!(!verify_schema(&doc.to_string()).issues.is_empty());
        }
        assert!(!merge_uninstall("").unwrap().changed);
    }

    #[test]
    fn jcode_verification_rejects_mixed_shim_paths() {
        let installed = merge_install("", &command()).unwrap();
        let mut doc = installed.content.parse::<DocumentMut>().unwrap();
        doc["hooks"]["pre_tool"] =
            toml_edit::value(hook_command(&std::env::temp_dir().join("other"), true).unwrap());
        assert!(!verify_schema(&doc.to_string()).issues.is_empty());
    }
}
