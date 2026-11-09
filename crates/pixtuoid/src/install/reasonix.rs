//! Reasonix hook install target: a plugin `reasonix plugin install --link` loads in
//! place from pixtuoid's config dir, through its native `reasonix-plugin.json`
//! (`docs/PLUGIN_PACKAGES.md` @ v2.39.0). A global plugin's hooks need no
//! `/hooks trust`, which project-scope hooks do.
//!
//! - `match` is OMITTED: empty = every tool. Any other value is an ANCHORED
//!   regex, and a malformed one never fires.
//! - `timeout` is in MILLISECONDS, and on the gating PreToolUse a TIMEOUT BLOCKS
//!   the user's tool call.
//! - EXEC form (`args` present): a command with neither `args` nor `shellCommand`
//!   that is not an absolute path resolves inside the plugin root
//!   (`internal/ext/hook/windows_posix_script.go` @ v2.39.0), so a shell-form
//!   `PIXTUOID_SOURCE=reasonix '<shim>'` would never run.
//! - No `_pixtuoid` sentinel: a v2 manifest rejects unknown fields.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::install::host_plugin::{self, NAME};
use crate::install::io;
use crate::install::merge;
use crate::install::target::{HostRegistration, LegacyConfig, MergeOutcome, Unregistered};
use crate::install::verify::{SchemaParse, ShimRef};

/// Events we register == events we decode, enforced by
/// `every_registered_reasonix_event_decodes` below. PostToolUseFailure /
/// StopFailure are deliberately ABSENT: the runner re-fires failures to NATIVE
/// hooks registered under PostToolUse / Stop with the event re-labeled, and ours
/// are native-format — registering both spellings would double-fire every failed
/// tool/turn for the same decoded end.
pub(crate) const REASONIX_EVENTS: &[&str] = &[
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "UserPromptSubmit",
    "Stop",
    "Notification",
    "SessionEnd",
    "SubagentStart",
    "SubagentStop",
];

/// `reasonix plugin install`'s flags: link in place, re-link over an existing
/// install.
const LINK: &str = "--link";
const REPLACE: &str = "--replace";
/// The consent to write that `install` and `remove` both require.
const YES: &str = "--yes";
/// The `reasonix plugin` subcommand that uninstalls one.
const REMOVE: &str = "remove";
/// The file Reasonix records its installed plugins in.
const PACKAGES: &str = "plugin-packages.json";

/// Reasonix links the plugin dir in place, so a rewrite of the manifest reaches the
/// next session.
pub(crate) const HOST: HostRegistration = HostRegistration {
    cli: CLI,
    register,
    unregister,
    is_registered,
    pickup: None,
    legacy: Some(LegacyConfig {
        path: settings_path,
        strip: merge::flat_json_uninstall,
    }),
};

const CLI: io::HostCli = io::HostCli {
    command: "reasonix",
};

const MANIFEST: &str = "reasonix-plugin.json";

const HOME_DIR: &str = ".reasonix";
const CONFIG_SUBDIR: &str = "reasonix";

const HOOKS_AT: host_plugin::HooksAt = host_plugin::HooksAt {
    hooks: "/contributes/hooks",
    command: "/command",
};

pub(crate) fn default_config_path() -> Result<PathBuf> {
    host_plugin::owned_path("reasonix-plugin", &[MANIFEST])
}

/// The GLOBAL `settings.json` a pixtuoid from before the plugin merged into.
fn settings_path() -> Result<PathBuf> {
    Ok(home()?.join("settings.json"))
}

/// Where Reasonix records its installed plugins and whether each is enabled
/// (`internal/ext/pluginpkg/pluginpkg.go` @ v2.39.0).
fn packages_path() -> Result<PathBuf> {
    Ok(home()?.join(PACKAGES))
}

/// Reasonix's home is platform-ASYMMETRIC, so a generic USERPROFILE-first path on
/// Windows misses it. Erroring mirrors the sibling home-anchored targets instead of
/// silently reading a CWD-relative file Reasonix never writes.
fn home() -> Result<PathBuf> {
    let config = if cfg!(windows) {
        "APPDATA/"
    } else if cfg!(target_os = "macos") {
        ""
    } else {
        "XDG_CONFIG_HOME/"
    };
    let read = format!(
        "REASONIX_HOME/{config}{}",
        pixtuoid_core::platform::USER_HOME_VAR
    );
    reasonix_home().ok_or_else(|| io::home_unset("Reasonix's home", &read))
}

/// `REASONIX_HOME`, else upstream's `defaultHomeDir`
/// (`internal/contract/config/storage_roots.go:252-270` @ v2.39.0, unchanged at
/// v2.58.1), with Go's `os.UserHomeDir` as the home.
fn reasonix_home() -> Option<PathBuf> {
    use pixtuoid_core::platform::{go_user_home_dir, path_env, text_env};
    let home = go_user_home_dir();
    let config_dir = if cfg!(windows) {
        path_env("APPDATA")
    } else if cfg!(target_os = "macos") {
        // Go's `os.UserConfigDir` there needs the `HOME` whose absence reached it.
        None
    } else {
        path_env("XDG_CONFIG_HOME")
    };
    resolve_reasonix_home(
        path_env("REASONIX_HOME").map(|v| expand_dir_value(&v, home.as_deref(), text_env)),
        cfg!(windows),
        config_dir,
        home,
    )
}

/// Upstream's `expandDirValue` (`internal/contract/config/paths.go:157-178` @
/// v2.39.0): trimmed, `${VAR}` substituted, then a leading `~` expanded.
fn expand_dir_value(
    raw: &Path,
    home: Option<&Path>,
    var: impl Fn(&str) -> Option<String>,
) -> PathBuf {
    match raw.to_str() {
        Some(s) => io::expand_tilde(Path::new(&expand_vars(s, var)), home),
        None => io::expand_tilde(raw, home),
    }
}

/// Upstream's `ExpandVars` (`internal/contract/config/expand.go:13-43` @
/// v2.39.0): `${NAME}` or `${NAME:-default}`, where an unset or empty variable
/// takes the default, else nothing; any other `$` stays literal.
fn expand_vars(s: &str, var: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("${") {
        let body = &rest[at + 2..];
        let Some(end) = body.find('}') else {
            break;
        };
        out.push_str(&rest[..at]);
        let (name, default) = body[..end]
            .split_once(":-")
            .map_or((&body[..end], None), |(name, d)| (name, Some(d)));
        let mut chars = name.chars();
        let named = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if named {
            let value = var(name).filter(|v| !v.is_empty());
            out.push_str(value.as_deref().or(default).unwrap_or_default());
            rest = &body[end + 1..];
        } else {
            out.push('$');
            rest = &rest[at + 1..];
        }
    }
    out.push_str(rest);
    out
}

/// Pure core for [`reasonix_home`] — everything is injected so BOTH platform arms
/// unit-test on any host. `config_dir` is what Go's `os.UserConfigDir` reads:
/// `%AppData%`, or a `$XDG_CONFIG_HOME` it refuses unless absolute (golang/go
/// `src/os/file.go:557-594` @ be57ebb0f3). `None` when upstream's is `""`, and
/// the legacy strip and the registration probe then error rather than read a
/// CWD-relative file Reasonix never writes.
fn resolve_reasonix_home(
    reasonix_home_env: Option<PathBuf>,
    windows: bool,
    config_dir: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(h) = reasonix_home_env {
        return Some(h);
    }
    let config_dir = config_dir.filter(|d| windows || d.is_absolute());
    if windows {
        return config_dir
            .or_else(|| home.map(|h| h.join("AppData").join("Roaming")))
            .map(|d| d.join(CONFIG_SUBDIR));
    }
    home.map(|h| h.join(HOME_DIR))
        .or_else(|| config_dir.map(|d| d.join(CONFIG_SUBDIR)))
}

/// Reasonix is present when its home dir is — and hook/trust users additionally
/// have a `~/.reasonix` even on Windows, so probe both.
pub(crate) fn detect_installed() -> bool {
    reasonix_home().is_some_and(|d| d.exists())
        || pixtuoid_core::platform::go_user_home_dir().is_some_and(|h| h.join(HOME_DIR).exists())
}

/// The shim's absolute path, exec form; the source rides as `--source` in `args`.
/// Err on non-UTF-8 (prevents the to_string_lossy dead-hook).
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    // `_explicit` is Claude's bare-name-vs-absolute switch — Reasonix always
    // embeds the absolute path, so the flag changes nothing here.
    merge::hook_path_str(resolved).map(str::to_string)
}

pub(crate) fn render_manifest(hook_cmd: &str) -> Result<String> {
    let entry = json!({
        "command": hook_cmd,
        "args": [
            crate::install::hook_cmd::SOURCE_FLAG.trim(),
            pixtuoid_core::source::reasonix::SOURCE_NAME,
        ],
        "timeout": 1000,
        "description": "pixtuoid visualizer",
    });
    let mut doc = manifest();
    doc["contributes"] = json!({ "hooks": host_plugin::hooks_map(REASONIX_EVENTS, &entry) });
    Ok(serde_json::to_string_pretty(&doc)?)
}

/// The manifest with nothing to contribute. No `version`: none is required, and one
/// would put the release in a file a launch compares byte for byte.
fn manifest() -> Value {
    json!({
        "apiVersion": "reasonix.io/plugin/v2",
        "name": NAME,
        "description": "Sends Reasonix's session events to the pixtuoid office",
    })
}

pub(crate) fn verify_schema(content: &str) -> SchemaParse {
    host_plugin::verify_hooks(content, HOOKS_AT, REASONIX_EVENTS, |c| {
        ShimRef::Absolute(PathBuf::from(c))
    })
}

pub(crate) fn installed_shim(content: &str) -> ShimRef {
    verify_schema(content).shim
}

/// The manifest as a disconnect leaves it: still one Reasonix loads, should the
/// plugin stay registered, and still naming us so `unregister` may delete it.
pub(crate) fn uninstall(content: &str) -> Result<MergeOutcome> {
    Ok(MergeOutcome {
        content: serde_json::to_string_pretty(&manifest())?,
        changed: host_plugin::holds_hooks(content, HOOKS_AT),
    })
}

/// `--yes` is the CLI's consent to write, not a trust step; `--replace` makes a
/// repeat connect re-link (`docs/PLUGIN_PACKAGES.md` @ v2.39.0).
fn register(config: &Path) -> Result<()> {
    let (plugin, _) = host_plugin::layout(config, &[MANIFEST])?;
    CLI.run(&[
        "plugin".as_ref(),
        "install".as_ref(),
        plugin.as_os_str(),
        LINK.as_ref(),
        REPLACE.as_ref(),
        YES.as_ref(),
    ])?;
    Ok(())
}

/// Reads Reasonix's state first, so a disconnect with nothing registered starts no
/// CLI. Removing a linked plugin leaves its dir (`docs/PLUGIN_PACKAGES.md`
/// @ v2.39.0), so this deletes it.
fn unregister(config: &Path) -> Result<Unregistered> {
    let (_, root) = host_plugin::layout(config, &[MANIFEST])?;
    let installed = plugin_entry()?.is_some();
    if installed {
        if CLI.locate().is_none() {
            tracing::warn!("reasonix not on PATH; leaving the pixtuoid plugin registered");
            return Ok(Unregistered::Unreachable);
        }
        CLI.run(&[
            "plugin".as_ref(),
            REMOVE.as_ref(),
            NAME.as_ref(),
            YES.as_ref(),
        ])?;
    }
    host_plugin::remove_if_ours(root, config, host_plugin::names_us)?;
    Ok(if installed {
        Unregistered::Removed
    } else {
        Unregistered::Absent
    })
}

fn is_registered() -> Result<bool> {
    Ok(plugin_entry()?.is_some_and(|p| p["enabled"].as_bool() == Some(true)))
}

/// Our row in Reasonix's installed plugins, if any.
fn plugin_entry() -> Result<Option<Value>> {
    let path = packages_path()?;
    let content = io::read_config(&path)?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    let state: Value =
        serde_json::from_str(&content).with_context(|| format!("parsing {}", path.display()))?;
    Ok(state["plugins"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["name"].as_str() == Some(NAME)))
        .cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::SENTINEL_KEY;

    fn json_merge_uninstall(doc: Value) -> Value {
        merge::flat_json_merge_uninstall(doc, SENTINEL_KEY)
    }

    /// Two events as a pixtuoid from before the plugin merged them into settings.json.
    fn legacy() -> Value {
        let ours = json!({
            SENTINEL_KEY: true,
            "command": "PIXTUOID_SOURCE=reasonix '/opt/pixtuoid-hook'",
            "timeout": 1000,
            "description": "pixtuoid visualizer"
        });
        json!({ "hooks": { "PreToolUse": [ours.clone()], "Stop": [ours] } })
    }

    #[test]
    fn reasonix_home_on_windows_is_appdata_then_the_roaming_dir_under_home() {
        let appdata = PathBuf::from(r"C:\Users\me\AppData\Roaming");
        let home = PathBuf::from(r"C:\Users\me");
        assert_eq!(
            resolve_reasonix_home(None, true, Some(appdata.clone()), Some(home.clone())),
            Some(appdata.join("reasonix"))
        );
        assert_eq!(
            resolve_reasonix_home(None, true, None, Some(home.clone())),
            Some(home.join("AppData").join("Roaming").join("reasonix"))
        );
        assert_eq!(resolve_reasonix_home(None, true, None, None), None);
    }

    #[test]
    fn reasonix_home_elsewhere_is_dot_reasonix_then_an_absolute_xdg_config_home() {
        let xdg = if cfg!(windows) {
            PathBuf::from(r"C:\xdg")
        } else {
            PathBuf::from("/xdg")
        };
        let home = PathBuf::from("/home/u");
        assert_eq!(
            resolve_reasonix_home(None, false, Some(xdg.clone()), Some(home.clone())),
            Some(home.join(".reasonix"))
        );
        assert_eq!(
            resolve_reasonix_home(None, false, Some(xdg.clone()), None),
            Some(xdg.join("reasonix"))
        );
        assert_eq!(
            resolve_reasonix_home(None, false, Some("rel/xdg".into()), None),
            None,
            "Go's os.UserConfigDir errors on a relative $XDG_CONFIG_HOME"
        );
        assert_eq!(resolve_reasonix_home(None, false, None, None), None);
    }

    #[test]
    fn reasonix_home_reads_its_platform_s_variables_and_names_them_when_none_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let (d, other) = (dir.path().join("d"), dir.path().join("other"));
        let vars = [
            "REASONIX_HOME",
            "APPDATA",
            "XDG_CONFIG_HOME",
            "HOME",
            "USERPROFILE",
        ];
        temp_env::with_vars_unset(vars, || {
            let err = home().unwrap_err().to_string();
            let listed: Vec<&str> = err
                .split_once('(')
                .and_then(|(_, rest)| rest.split_once(" unset)"))
                .map(|(list, _)| list.split('/').collect())
                .unwrap_or_default();
            let read: &[&str] = if cfg!(windows) {
                &["REASONIX_HOME", "APPDATA", "USERPROFILE"]
            } else if cfg!(target_os = "macos") {
                &["REASONIX_HOME", "HOME"]
            } else {
                &["REASONIX_HOME", "XDG_CONFIG_HOME", "HOME"]
            };
            assert_eq!(
                listed, read,
                "names exactly what this platform reads: {err}"
            );
        });
        #[cfg(windows)]
        {
            temp_env::with_vars(
                [
                    ("REASONIX_HOME", None),
                    ("APPDATA", Some(&d)),
                    ("USERPROFILE", Some(&other)),
                ],
                || assert_eq!(reasonix_home(), Some(d.join("reasonix"))),
            );
            temp_env::with_vars(
                [
                    ("REASONIX_HOME", None),
                    ("APPDATA", None),
                    ("USERPROFILE", None),
                    ("HOME", Some(&other)),
                ],
                || assert_eq!(reasonix_home(), None, "Go reads no HOME on Windows"),
            );
        }
        #[cfg(not(windows))]
        {
            temp_env::with_vars(
                [
                    ("REASONIX_HOME", None),
                    ("HOME", Some(&d)),
                    ("XDG_CONFIG_HOME", Some(&other)),
                    ("APPDATA", Some(&other)),
                ],
                || assert_eq!(reasonix_home(), Some(d.join(".reasonix"))),
            );
            temp_env::with_vars(
                [
                    ("REASONIX_HOME", None),
                    ("HOME", None),
                    ("XDG_CONFIG_HOME", Some(&d)),
                ],
                || {
                    let want = (!cfg!(target_os = "macos")).then(|| d.join("reasonix"));
                    assert_eq!(reasonix_home(), want);
                },
            );
        }
    }

    #[test]
    fn reasonix_home_env_override_wins_verbatim_on_both_platforms() {
        for windows in [true, false] {
            assert_eq!(
                resolve_reasonix_home(
                    Some("/custom/rx".into()),
                    windows,
                    Some(PathBuf::from(r"C:\AppData")),
                    Some("/home/u".into()),
                ),
                Some(PathBuf::from("/custom/rx"))
            );
        }
    }

    #[test]
    fn reasonix_home_env_expands_vars_then_tilde_as_upstream_does() {
        let home = Path::new("/home/u");
        let var = |name: &str| match name {
            "X" => Some("/x".to_owned()),
            "EMPTY" => Some(String::new()),
            _ => None,
        };
        for (raw, want) in [
            ("  ~  ", home.to_path_buf()),
            ("~/rx", home.join("rx")),
            ("${X}/rx", PathBuf::from("/x/rx")),
            ("${UNSET:-~/d}/rx", home.join("d").join("rx")),
            ("${EMPTY:-/e}", PathBuf::from("/e")),
            ("${UNSET}/rx", PathBuf::from("/rx")),
            ("${1X}${X}", PathBuf::from("${1X}/x")),
            ("${X-d}", PathBuf::from("${X-d}")),
            ("$X/rx", PathBuf::from("$X/rx")),
            ("${X", PathBuf::from("${X")),
            ("~user/rx", PathBuf::from("~user/rx")),
        ] {
            assert_eq!(
                expand_dir_value(Path::new(raw), Some(home), var),
                want,
                "{raw}"
            );
        }
    }

    #[test]
    fn uninstall_removes_only_managed_entries_and_empty_maps() {
        let mut installed = legacy();
        installed["hooks"]["PreToolUse"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({ "match": "bash", "command": "my-guard.sh" }));
        let cleaned = json_merge_uninstall(installed);
        let arr = cleaned["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["command"], json!("my-guard.sh"));
        for ev in REASONIX_EVENTS.iter().filter(|e| **e != "PreToolUse") {
            assert!(
                cleaned["hooks"].get(*ev).is_none(),
                "event {ev} should be dropped once empty"
            );
        }
    }

    #[test]
    fn uninstall_all_managed_drops_hooks_map() {
        let cleaned = json_merge_uninstall(legacy());
        assert!(cleaned.get("hooks").is_none(), "got {cleaned}");
    }

    #[test]
    fn every_event_execs_the_shim_with_its_source_and_reads_it_back() {
        let shim = if cfg!(windows) {
            r"C:\Program Files\pixtuoid-hook.exe"
        } else {
            "/Users/Jane Doe/bin/pixtuoid-hook"
        };
        let cmd = hook_command(Path::new(shim), false).unwrap();
        assert_eq!(cmd, shim, "exec form: the bare path, spaces and all");
        let rendered = render_manifest(&cmd).unwrap();
        let doc: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(doc["apiVersion"], json!("reasonix.io/plugin/v2"));
        let mut keys: Vec<_> = doc.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["apiVersion", "contributes", "description", "name"],
            "a v2 manifest rejects unknown fields"
        );
        let hooks = doc["contributes"]["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), REASONIX_EVENTS.len());
        for ev in REASONIX_EVENTS {
            assert_eq!(
                hooks[*ev],
                json!([{
                    "command": shim,
                    "args": ["--source", "reasonix"],
                    "timeout": 1000,
                    "description": "pixtuoid visualizer",
                }]),
                "{ev}"
            );
        }
        let v = verify_schema(&rendered);
        assert!(v.issues.is_empty(), "{:?}", v.issues);
        assert_eq!(v.shim, ShimRef::Absolute(PathBuf::from(shim)));
    }

    #[test]
    fn a_disconnected_manifest_still_loads_and_names_us() {
        let out = uninstall(&render_manifest("/x/pixtuoid-hook").unwrap()).unwrap();
        assert!(out.changed);
        let doc: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(doc, manifest());
        assert!(!uninstall(&out.content).unwrap().changed);
        assert!(!uninstall("").unwrap().changed);
    }

    #[test]
    fn merge_uninstall_no_pixtuoid_hooks_reports_unchanged() {
        let user = r#"{ "hooks": { "Stop": [ { "command": "notify-send done" } ] } }"#;
        let out = merge::flat_json_uninstall(user).unwrap();
        assert!(!out.changed, "no managed entries → semantic no-op");
    }

    #[test]
    #[cfg(unix)]
    fn hook_command_errors_on_non_utf8_path() {
        use std::os::unix::ffi::OsStrExt;
        let bad = Path::new(std::ffi::OsStr::from_bytes(b"/x/\xff/pixtuoid-hook"));
        assert!(hook_command(bad, false).is_err());
    }

    #[test]
    fn every_registered_reasonix_event_decodes() {
        use pixtuoid_core::source::decoder::decode_hook_payload;
        for ev in REASONIX_EVENTS {
            let payload = serde_json::json!({
                "event": ev,
                "sessionId": "s",
                "cwd": "/repo",
                "_pixtuoid_source": "reasonix",
            });
            assert!(
                decode_hook_payload(payload).is_ok(),
                "registered Reasonix hook {ev:?} has no decoder arm — it would \
                 bail as unsupported. Add an arm in pixtuoid-core source/reasonix.rs."
            );
        }
    }

    #[test]
    fn reasonix_events_pins_the_exact_registered_set() {
        crate::install::assert_event_roster(
            "REASONIX_EVENTS",
            REASONIX_EVENTS,
            &[
                "SessionStart",
                "PreToolUse",
                "PostToolUse",
                "PermissionRequest",
                "UserPromptSubmit",
                "Stop",
                "Notification",
                "SessionEnd",
                "SubagentStart",
                "SubagentStop",
            ],
        );
    }
}
