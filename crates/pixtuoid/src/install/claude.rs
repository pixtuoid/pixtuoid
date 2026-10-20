use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pixtuoid_core::source::claude_code::claude_config_dir;
use serde_json::{Value, json};

use crate::install::host_plugin::{self, NAME};
use crate::install::io;
use crate::install::merge;
use crate::install::target::{HostRegistration, LegacyConfig, Unregistered};
use crate::install::verify::ShimRef;

pub(crate) const EVENTS: &[&str] = &[
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    // A tool gate arrives as `PermissionRequest`. The decoder has read this
    // event since Codex needed it — unregistered, the bytes never came.
    "PermissionRequest",
    // The ONLY end signal a Workflow-fleet subagent gets: no per-agent Agent
    // tool_use, no transcript end marker.
    "SubagentStart",
    "SubagentStop",
    "SessionEnd",
];

/// The settings key a user-scope install is recorded under.
const ENABLED_PLUGINS: &str = "enabledPlugins";
/// The `claude plugin` subcommand that installs one.
const INSTALL: &str = "install";

/// Claude Code loads the hooks through its plugin system
/// (code.claude.com/docs/en/plugins): a local marketplace whose one plugin loads
/// in place, so a rewrite of the file reaches the next session.
pub(crate) const HOST: HostRegistration = HostRegistration {
    cli: CLI,
    register,
    unregister,
    is_registered,
    copy: None,
    legacy: Some(LegacyConfig {
        path: settings_path,
        strip: merge::flat_json_uninstall,
    }),
};

const CLI: io::HostCli = io::HostCli { command: "claude" };

pub(crate) fn default_config_path() -> Result<PathBuf> {
    host_plugin::owned_path("claude-plugin", host_plugin::HOOKS_FILE)
}

/// Claude Code's user settings, where it records the plugin as enabled.
fn settings_path() -> Result<PathBuf> {
    if let Some(dir) = claude_config_dir() {
        return Ok(dir.join("settings.json"));
    }
    io::home_relative_checked(".claude/settings.json")
}

/// Claude Code is present when its config dir exists; the hooks file is ours, so
/// its absence says nothing.
pub(crate) fn detect_installed() -> bool {
    settings_path()
        .ok()
        .and_then(|p| p.parent().map(Path::is_dir))
        .unwrap_or(false)
}

fn marketplace_manifest(root: &Path) -> PathBuf {
    root.join(".claude-plugin").join("marketplace.json")
}

fn register(config: &Path) -> Result<()> {
    let (plugin, root) = host_plugin::layout(config, host_plugin::HOOKS_FILE)?;
    host_plugin::write_manifests(&[
        (
            marketplace_manifest(root),
            json!({
                "name": NAME,
                "owner": { "name": NAME },
                "plugins": [{
                    "name": NAME,
                    "source": host_plugin::relative_source(plugin)?,
                    "description": "Sends Claude Code's session events to the pixtuoid office",
                }],
            }),
        ),
        (
            plugin.join(".claude-plugin").join("plugin.json"),
            json!({
                "name": NAME,
                "version": env!("CARGO_PKG_VERSION"),
                "description": "Sends Claude Code's session events to the pixtuoid office",
                "homepage": env!("CARGO_PKG_HOMEPAGE"),
            }),
        ),
    ])?;
    // Both are idempotent: a repeat add reports the marketplace already on disk,
    // a repeat install that the plugin already loads in place.
    CLI.run(&[
        "plugin".as_ref(),
        "marketplace".as_ref(),
        "add".as_ref(),
        root.as_os_str(),
    ])?;
    CLI.run(&[
        "plugin".as_ref(),
        INSTALL.as_ref(),
        host_plugin::plugin_id().as_ref(),
    ])?;
    Ok(())
}

/// Removing the marketplace uninstalls its plugins too. `remove` exits non-zero on
/// a marketplace it doesn't know, hence the list first.
fn unregister(config: &Path) -> Result<Unregistered> {
    if CLI.locate().is_none() {
        // Disabled is still installed: its marketplace must outlive it.
        if plugin_entry()?.is_some() {
            tracing::warn!("claude not on PATH; leaving the pixtuoid plugin registered");
            return Ok(Unregistered::Unreachable);
        }
        let (_, root) = host_plugin::layout(config, host_plugin::HOOKS_FILE)?;
        host_plugin::remove_if_ours(root, &marketplace_manifest(root))?;
        return Ok(Unregistered::Absent);
    }
    let listed = CLI.run(&[
        "plugin".as_ref(),
        "marketplace".as_ref(),
        "list".as_ref(),
        "--json".as_ref(),
    ])?;
    let found = marketplace_listed(&listed)?;
    let (_, root) = host_plugin::layout(config, host_plugin::HOOKS_FILE)?;
    if found {
        CLI.run(&[
            "plugin".as_ref(),
            "marketplace".as_ref(),
            "remove".as_ref(),
            NAME.as_ref(),
        ])?;
    }
    host_plugin::remove_if_ours(root, &marketplace_manifest(root))?;
    Ok(if found {
        Unregistered::Removed
    } else {
        Unregistered::Absent
    })
}

/// Read where Claude Code records a user-scope install as enabled
/// (code.claude.com/docs/en/plugins/install#choose-an-install-scope) — the settings
/// file, not `claude plugin list`, which costs a CLI start per check.
fn is_registered() -> Result<bool> {
    Ok(plugin_entry()? == Some(true))
}

/// The plugin's `enabledPlugins` value: `Some(false)` is installed but disabled.
fn plugin_entry() -> Result<Option<bool>> {
    Ok(read_settings(&settings_path()?)?[ENABLED_PLUGINS][host_plugin::plugin_id()].as_bool())
}

/// `Null` for a missing or empty file.
fn read_settings(settings: &Path) -> Result<Value> {
    let content = io::read_config(settings)?;
    if content.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&content).with_context(|| format!("parsing {}", settings.display()))
}

fn marketplace_listed(json_out: &str) -> Result<bool> {
    let rows: Vec<Value> = serde_json::from_str(json_out)
        .context("parsing `claude plugin marketplace list --json`")?;
    Ok(rows
        .iter()
        .any(|r| r.get("name").and_then(Value::as_str) == Some(NAME)))
}

/// Unix: the bare name behind the `PIXTUOID_SOURCE=` prefix every other source
/// carries, so CC PATH-resolves it and a binary upgrade applies without a
/// rewrite. [`io::HOOK_OVERRIDE_ENV`] overrides that — the user set it precisely because
/// the binary is off-PATH — and is single-quoted, since CC runs shell-form
/// commands through a shell.
///
/// Windows: exec form requires the absolute PE path (shell-form goes through
/// cmd.exe/PowerShell — unportable, PATHEXT-dependent). The orchestrator hard-errors if
/// resolution failed, so `resolved` is guaranteed absolute here.
pub(crate) fn hook_command(resolved: &Path, explicit: bool) -> Result<String> {
    #[cfg(not(windows))]
    {
        // CC's Unix entry carries no `args` key, so it runs through a SHELL and
        // the env prefix every other source uses works here too. It was bare for
        // years, which made "no `_pixtuoid_source`" mean "probably CC" — the
        // guess `decoder` still has to make for installs written before this, and
        // the reason an unstamped cursor invocation landed on CC's arms at all.
        let p = if explicit {
            merge::hook_path_str(resolved)?
        } else {
            io::HOOK_BIN
        };
        crate::install::hook_cmd::shell_hook_command(
            p,
            pixtuoid_core::source::claude_code::SOURCE_NAME,
        )
    }
    #[cfg(windows)]
    {
        let _ = explicit; // exec form always embeds the absolute path
        merge::hook_path_str(resolved).map(str::to_string)
    }
}

/// The inner hook object of a hooks.json entry. `exec_form` adds the empty `args` key
/// that makes CC spawn the PE directly instead of through a shell; `matcher` lives
/// on the OUTER entry, not here.
pub(crate) fn hook_entry(cmd: &str, exec_form: bool) -> Value {
    if exec_form {
        json!({ "type": "command", "command": cmd, "args": [] })
    } else {
        json!({ "type": "command", "command": cmd })
    }
}

pub(crate) fn verify_schema(content: &str) -> crate::install::verify::SchemaParse {
    host_plugin::verify_hooks(content, host_plugin::CLAUDE_SHAPE, EVENTS, claude_shim_ref)
}

fn claude_shim_ref(command: &str) -> ShimRef {
    // The SHELL form is `shell_shim_ref`'s own wire format, so it parses it —
    // including any future ` --event` suffix, which a private copy here would
    // bake into the path. CC alone can answer `BareName`, so that is the one
    // thing mapped on top.
    #[cfg(not(windows))]
    {
        match crate::install::verify::shell_shim_ref(command.trim()) {
            ShimRef::Absolute(p) if p.as_os_str() == io::HOOK_BIN => ShimRef::BareName,
            other => other,
        }
    }
    // NOT that parser: CC's Windows entry is exec form (`args: []`), a bare
    // absolute path with neither quoting nor `--source`, so it would fall to
    // that parser's last-whitespace-token arm and truncate `C:\Program Files\…`.
    #[cfg(windows)]
    {
        ShimRef::Absolute(PathBuf::from(command.trim()))
    }
}

pub(crate) fn installed_shim(content: &str) -> ShimRef {
    verify_schema(content).shim
}

pub(crate) fn render_hooks(hook_cmd: &str) -> Result<String> {
    host_plugin::render_hooks(EVENTS, &managed_entry(hook_cmd))
}

/// The one place Claude's nested per-event shape lives.
fn managed_entry(hook_command: &str) -> Value {
    json!({
        "matcher": ".*",
        "hooks": [ hook_entry(hook_command, cfg!(windows)) ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_path_honors_claude_config_dir() {
        let fallback_suffix = PathBuf::from(".claude").join("settings.json");

        temp_env::with_var_unset("CLAUDE_CONFIG_DIR", || {
            let unset_path = settings_path().unwrap();
            assert!(
                unset_path.ends_with(&fallback_suffix),
                "default config path must end with .claude/settings.json, got {unset_path:?}"
            );
        });

        let custom_dir = std::env::temp_dir().join("pixtuoid-claude-config-dir");
        temp_env::with_var("CLAUDE_CONFIG_DIR", Some(&custom_dir), || {
            assert_eq!(settings_path().unwrap(), custom_dir.join("settings.json"));
        });

        temp_env::with_var("CLAUDE_CONFIG_DIR", Some(""), || {
            let empty_path = settings_path().unwrap();
            assert!(
                empty_path.ends_with(&fallback_suffix),
                "empty CLAUDE_CONFIG_DIR must fall back to .claude/settings.json, got {empty_path:?}"
            );
        });
    }

    #[test]
    fn the_hooks_file_sits_in_pixtuoids_own_marketplace() {
        let base = std::env::temp_dir().join("pixtuoid-xdg");
        temp_env::with_var("XDG_CONFIG_HOME", Some(&base), || {
            let hooks = default_config_path().unwrap();
            assert_eq!(
                hooks,
                base.join("pixtuoid/claude-plugin/pixtuoid/hooks/hooks.json")
            );
        });
    }

    #[test]
    fn the_marketplace_listing_is_read_by_name() {
        assert!(marketplace_listed(r#"[{"name":"other"},{"name":"pixtuoid"}]"#).unwrap());
        assert!(!marketplace_listed(r#"[{"name":"other"}]"#).unwrap());
        assert!(marketplace_listed("not json").is_err());
    }

    #[test]
    fn install_writes_every_event() {
        let first = render_hooks("/usr/local/bin/pixtuoid-hook").unwrap();
        let doc: Value = serde_json::from_str(&first).unwrap();
        let hooks = doc["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), EVENTS.len());
        for ev in EVENTS {
            assert_eq!(
                hooks[*ev][0]["hooks"][0]["command"],
                json!("/usr/local/bin/pixtuoid-hook")
            );
            assert!(
                hooks[*ev][0].get(crate::install::SENTINEL_KEY).is_none(),
                "Claude Code rejects unknown keys in a plugin's hooks.json"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn hook_command_explicit_path_is_embedded_and_stamped_on_unix() {
        let cmd = hook_command(Path::new("/opt/custom/pixtuoid-hook"), true).unwrap();
        assert_eq!(
            cmd,
            "PIXTUOID_SOURCE=claude-code '/opt/custom/pixtuoid-hook'"
        );
        let spaced = hook_command(Path::new("/Users/Jane Doe/bin/pixtuoid-hook"), true).unwrap();
        assert_eq!(
            spaced,
            "PIXTUOID_SOURCE=claude-code '/Users/Jane Doe/bin/pixtuoid-hook'"
        );
    }

    #[cfg(unix)]
    #[test]
    fn hook_command_auto_resolved_carries_the_source_on_unix() {
        let cmd = hook_command(Path::new("/usr/local/bin/pixtuoid-hook"), false).unwrap();
        assert_eq!(cmd, "PIXTUOID_SOURCE=claude-code 'pixtuoid-hook'");
    }

    #[cfg(unix)]
    #[test]
    fn claude_shim_ref_reads_the_auto_resolved_command_back_as_a_bare_name() {
        let cmd = hook_command(Path::new("/usr/local/bin/pixtuoid-hook"), false).unwrap();
        assert_eq!(claude_shim_ref(&cmd), ShimRef::BareName);
    }

    #[cfg(unix)]
    #[test]
    fn claude_shim_ref_survives_an_event_suffix_it_does_not_write_today() {
        assert_eq!(
            claude_shim_ref("PIXTUOID_SOURCE=claude-code '/opt/pixtuoid-hook' --event PreToolUse"),
            ShimRef::Absolute(std::path::PathBuf::from("/opt/pixtuoid-hook"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn claude_shim_ref_recovers_a_single_quoted_path_with_an_apostrophe() {
        use crate::install::hook_cmd::unix::shell_single_quote;
        let path = "/U/O'B/pixtuoid-hook";
        let cmd = shell_single_quote(path);
        assert!(
            cmd.contains("'\\''"),
            "expected an escaped apostrophe in {cmd:?}"
        );
        assert_eq!(
            claude_shim_ref(&cmd),
            ShimRef::Absolute(std::path::PathBuf::from(path))
        );
    }

    #[test]
    fn claude_shim_ref_half_quoted_command_is_literal_not_unquoted() {
        assert_eq!(
            claude_shim_ref("'/opt/pixtuoid-hook"),
            ShimRef::Absolute(std::path::PathBuf::from("'/opt/pixtuoid-hook"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn hook_command_embeds_absolute_path_on_windows_either_way() {
        for explicit in [true, false] {
            let cmd = hook_command(Path::new(r"C:\tools\pixtuoid-hook.exe"), explicit).unwrap();
            assert_eq!(cmd, r"C:\tools\pixtuoid-hook.exe");
        }
    }

    #[test]
    fn windows_entry_is_exec_form_with_absolute_path() {
        let entry = hook_entry(r"C:\Users\user\.cargo\bin\pixtuoid-hook.exe", true);
        assert_eq!(entry["type"], json!("command"));
        assert_eq!(
            entry["command"],
            json!(r"C:\Users\user\.cargo\bin\pixtuoid-hook.exe")
        );
        assert_eq!(
            entry["args"],
            json!([]),
            "exec form must carry args:[] for shell-free spawn"
        );
    }

    #[test]
    fn unix_entry_stays_bare_shell_form() {
        let entry = hook_entry("pixtuoid-hook", false);
        assert_eq!(entry["type"], json!("command"));
        assert_eq!(entry["command"], json!("pixtuoid-hook"));
        assert!(
            entry.get("args").is_none(),
            "unix shell-form must NOT carry an args key (was: {entry})"
        );
    }

    #[test]
    fn every_registered_cc_event_decodes() {
        use pixtuoid_core::source::decoder::decode_hook_payload;
        for ev in EVENTS {
            let payload = serde_json::json!({
                "hook_event_name": ev,
                "session_id": "sess",
                "transcript_path": "/p/sess.jsonl",
                "cwd": "/repo",
                // Required by the SubagentStart/Stop arms; inert for every other event.
                "agent_id": "a0000000000000001",
            });
            assert!(
                decode_hook_payload(payload).is_ok(),
                "registered CC hook {ev:?} has no decoder arm — it would bail as \
                 unsupported. Add an arm in pixtuoid-core (decoder.rs shared arms \
                 or claude_code.rs's custom decoder)."
            );
        }
    }

    #[test]
    fn claude_events_pins_the_exact_registered_set() {
        crate::install::assert_event_roster(
            "EVENTS",
            EVENTS,
            &[
                "SessionStart",
                "PreToolUse",
                "PostToolUse",
                "Notification",
                "PermissionRequest",
                "SubagentStart",
                "SubagentStop",
                "SessionEnd",
            ],
        );
    }
}
