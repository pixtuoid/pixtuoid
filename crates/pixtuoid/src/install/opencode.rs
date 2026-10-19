//! opencode hook install target — a TS PLUGIN, not a config block.
//!
//! opencode has no config-level shell hook (and SQLite-only sessions, no
//! tailable transcript), so pixtuoid drops a plugin file at
//! `<opencode-config>/plugins/pixtuoid.ts`, which opencode auto-discovers. The
//! plugin pipes the session/tool/permission event stream into the
//! `pixtuoid-hook` shim on stdin; the shim's absolute path is baked in
//! (JSON-escaped) at install time from the `opencode_plugin.ts` template.
//!
//! The plugin FILE is the target's owned primary, so install renders the whole
//! file and a disconnect replaces it with a sentinel-free no-op plugin: the
//! orchestrator's `write_atomic` can't delete.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::install::io;
use crate::install::target::MergeOutcome;

/// Marks the plugin as ours — absent from the removed-stub, so an uninstall of
/// a foreign/removed file is a clean no-op.
const SENTINEL: &str = "@pixtuoid-opencode-plugin";

const HOOK_PLACEHOLDER: &str = "\"{{HOOK_PATH_JSON}}\"";

const PLUGIN_TEMPLATE: &str = include_str!("opencode_plugin.ts");

/// The events the plugin forwards: its `FORWARD` set.
#[cfg(test)]
pub(crate) fn plugin_events() -> Vec<&'static str> {
    crate::install::forward_set(PLUGIN_TEMPLATE)
        .into_iter()
        .collect()
}

/// A no-op plugin WITHOUT the sentinel, so a re-uninstall is a no-op.
pub(crate) const REMOVED_STUB: &str = "// pixtuoid opencode plugin removed by disconnecting opencode in pixtuoid's Sources panel (press s).\nexport default { id: \"pixtuoid-removed\", setup() {} }\n";

/// What pixtuoid's disconnect wrote before it supported opencode 2, which
/// opencode 2 cannot load.
pub(crate) const V1_REMOVED_STUB: &str = "// pixtuoid opencode plugin removed by disconnecting opencode in pixtuoid's Sources panel (press s).\nexport {}\n";

fn opencode_config_dir() -> Result<PathBuf> {
    config_dir_from(
        pixtuoid_core::platform::path_env("OPENCODE_CONFIG_DIR").as_deref(),
        pixtuoid_core::platform::path_env("XDG_CONFIG_HOME").as_deref(),
        pixtuoid_core::platform::user_home_opt().as_deref(),
    )
}

/// Mirrors opencode's own precedence (`packages/util/src/global.ts:79`,
/// `global-roots.ts:7`), so we write into the dir it actually scans for
/// plugins: `OPENCODE_CONFIG_DIR`, then `$XDG_CONFIG_HOME/opencode`, then
/// `<home>/.config/opencode`.
fn config_dir_from(oc: Option<&Path>, xdg: Option<&Path>, home: Option<&Path>) -> Result<PathBuf> {
    // Blank overrides were already filtered at the read (`platform::path_env`).
    if let Some(dir) = oc {
        return Ok(dir.to_path_buf());
    }
    if let Some(xdg) = xdg {
        return Ok(xdg.join("opencode"));
    }
    home.map(|h| h.join(".config").join("opencode"))
        .ok_or_else(|| io::home_unset("the home directory", "HOME/USERPROFILE"))
}

/// opencode auto-discovers `<config>/{plugin,plugins}/*.{ts,js}`
/// (`packages/core/src/plugin/source-directory.ts:7`).
pub(crate) fn default_config_path() -> Result<PathBuf> {
    Ok(opencode_config_dir()?.join("plugins").join("pixtuoid.ts"))
}

/// Presence probe for auto-detect: probe opencode's OWN dirs, NOT our plugin
/// file — keying on our own artifact would chicken-and-egg (opencode could never
/// be auto-detected until AFTER we'd installed into it).
pub(crate) fn detect_installed() -> bool {
    opencode_config_dir().map(|d| d.exists()).unwrap_or(false)
        || io::home_relative(".local/share/opencode").exists()
}

/// The plugin spawns the shim by embedded path (no PATH reliance), so
/// `_explicit` — Claude's bare-vs-absolute switch — is irrelevant here:
/// opencode always needs the absolute path.
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    crate::install::merge::hook_path_str(resolved).map(str::to_string)
}

/// `changed` only when the content is actually ours (carries the sentinel) — a
/// foreign file, an already-removed stub, or empty content is left untouched.
pub(crate) fn uninstall(content: &str) -> Result<MergeOutcome> {
    let ours = content.contains(SENTINEL);
    Ok(MergeOutcome {
        changed: ours,
        content: if ours {
            REMOVED_STUB.to_string()
        } else {
            content.to_string()
        },
    })
}

/// The managed plugin is a CODE artifact, so there is no per-event config to
/// check — only that the sentinel is present, the shim-path placeholder was
/// substituted, and the baked `HOOK_PATH` is readable for the on-disk stat.
pub(crate) fn verify_schema(content: &str) -> crate::install::verify::SchemaParse {
    use crate::install::verify::{SchemaParse, ShimRef};
    if !content.contains(SENTINEL) {
        return SchemaParse::broken(
            "the opencode plugin is missing or replaced (sentinel absent) — reconnect opencode",
        );
    }
    if content.contains(HOOK_PLACEHOLDER) {
        return SchemaParse::broken(
            "the opencode plugin's shim-path placeholder was never substituted",
        );
    }
    let Some(p) = crate::install::verify::baked_hook_path(content) else {
        return SchemaParse::broken("could not read HOOK_PATH from the opencode plugin");
    };
    SchemaParse {
        shim: ShimRef::Absolute(p),
        ..Default::default()
    }
}

pub(crate) fn render_plugin(hook_path: &str) -> Result<String> {
    crate::install::merge::bake_hook_path(PLUGIN_TEMPLATE, HOOK_PLACEHOLDER, hook_path, "opencode")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every event name the plugin spells outside `FORWARD`, wherever it
    /// compares an event, must be one it forwards: the loop drops every other.
    #[test]
    fn every_event_the_plugin_names_is_one_it_forwards() {
        let forwarded = crate::install::forward_set(PLUGIN_TEMPLATE);
        for lit in PLUGIN_TEMPLATE.split('"').skip(1).step_by(2) {
            let event_shaped = lit.contains('.')
                && lit.split('.').all(|w| {
                    !w.is_empty()
                        && w.bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                });
            if event_shaped {
                assert!(
                    forwarded.contains(lit),
                    "the plugin names `{lit}` but never forwards it"
                );
            }
        }
    }

    /// The plugin forwards EXACTLY what the decoder reads. These are two lists in
    /// two languages — a TS `FORWARD` set here, `match event` arms in
    /// `pixtuoid-core` — and no compiler can see across that gap, so they are
    /// bound through core's `DECODED_EVENTS` instead. Forward one the decoder
    /// ignores and we spawn a shim per event for nothing; miss one it reads and
    /// that signal never arrives.
    #[test]
    fn the_plugin_forwards_exactly_what_the_decoder_reads() {
        let forwarded: std::collections::BTreeSet<&str> = plugin_events().into_iter().collect();
        let decoded: std::collections::BTreeSet<&str> =
            pixtuoid_core::source::opencode::DECODED_EVENTS
                .iter()
                .copied()
                .collect();

        assert_eq!(forwarded, decoded);
    }

    /// The `data` paths `allowlisted` copies, dotted, read out of the template.
    /// A line in any form but the two below panics, so a field cannot ride a
    /// line in unseen or copy something other than its own name.
    fn allowlisted_fields() -> std::collections::BTreeSet<String> {
        let block = PLUGIN_TEMPLATE
            .split_once("const allowlisted = (data: any) => ({")
            .and_then(|(_, rest)| rest.split_once("})"))
            .map(|(inner, _)| inner)
            .expect("the template defines allowlisted");
        block
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| {
                let (key, _) = line.split_once(':').unwrap_or(("", ""));
                if line == format!("{key}: data?.{key},") {
                    return key.to_owned();
                }
                let sub = line
                    .split_once("&& { ")
                    .and_then(|(_, inner)| inner.split_once(':'))
                    .map_or("", |(sub, _)| sub);
                assert_eq!(
                    line,
                    format!("{key}: data?.{key} && {{ {sub}: data.{key}.{sub} }},"),
                    "an allowlisted line copies something other than its own path"
                );
                format!("{key}.{sub}")
            })
            .collect()
    }

    /// The plugin strips every field it does not copy, and the decoder's own
    /// tests build their payloads inline, so the two field lists are pinned
    /// here, as the event lists are above.
    #[test]
    fn the_plugin_copies_exactly_the_fields_the_decoder_reads() {
        let decoded: std::collections::BTreeSet<String> =
            pixtuoid_core::source::opencode::DECODED_DATA_FIELDS
                .iter()
                .map(ToString::to_string)
                .collect();
        assert_eq!(allowlisted_fields(), decoded);
        assert!(
            PLUGIN_TEMPLATE
                .contains("location: event.location && { directory: event.location.directory }"),
            "the decoder reads the envelope's location.directory as an event's cwd"
        );
    }

    #[test]
    fn verify_schema_reports_the_baked_shim_and_every_way_the_plugin_can_be_dead() {
        use crate::install::verify::ShimRef;

        let installed = render_plugin("/opt/bin/pixtuoid-hook").unwrap();
        let sound = verify_schema(&installed);
        assert!(
            sound.issues.is_empty(),
            "a freshly rendered plugin is sound — got {:?}",
            sound.issues
        );
        assert_eq!(
            sound.shim,
            ShimRef::Absolute(std::path::PathBuf::from("/opt/bin/pixtuoid-hook")),
            "the baked HOOK_PATH must be reported, or the shim-on-disk check is skipped"
        );

        let foreign = verify_schema("export default function () {}\n");
        assert_eq!(foreign.shim, ShimRef::Unknown);
        assert!(
            foreign
                .issues
                .iter()
                .any(|i| i.contains("reconnect opencode")),
            "a sentinel-less file must be a HARD issue naming the remedy — got {:?}",
            foreign.issues
        );

        let unsubstituted = format!("// {SENTINEL}\nconst HOOK_PATH = {HOOK_PLACEHOLDER};\n");
        assert!(
            verify_schema(&unsubstituted)
                .issues
                .iter()
                .any(|i| i.contains("placeholder")),
            "an unsubstituted placeholder must be reported"
        );

        let no_binding = format!("// {SENTINEL}\nexport default function () {{}}\n");
        assert!(
            verify_schema(&no_binding)
                .issues
                .iter()
                .any(|i| i.contains("HOOK_PATH")),
            "an unreadable HOOK_PATH must be reported, not silently accepted"
        );
    }

    #[test]
    fn install_bakes_the_hook_path_and_carries_the_sentinel() {
        let out = render_plugin("/opt/bin/pixtuoid-hook").unwrap();
        assert!(
            out.contains(SENTINEL),
            "rendered plugin must carry the sentinel"
        );
        assert!(out.contains("\"/opt/bin/pixtuoid-hook\""));
        assert!(
            !out.contains(HOOK_PLACEHOLDER),
            "placeholder must be replaced"
        );
        assert!(
            out.contains("--source"),
            "spawns the shim with --source opencode"
        );
    }

    #[test]
    fn rendered_hook_binding_round_trips_escaped_paths() {
        for path in [
            r"C:\Program Files\Pixtuoid\pixtuoid-hook.exe",
            r#"/tmp/"quoted"/pixtuoid-hook"#,
        ] {
            let rendered = render_plugin(path).unwrap();
            let binding = rendered
                .lines()
                .find(|line| line.starts_with("const HOOK_PATH: string = "))
                .unwrap();
            let encoded = binding.strip_prefix("const HOOK_PATH: string = ").unwrap();
            let expected_json = serde_json::to_string(path).unwrap();
            assert_eq!(
                binding,
                format!("const HOOK_PATH: string = {expected_json}")
            );
            assert_eq!(serde_json::from_str::<String>(encoded).unwrap(), path);
        }
    }

    #[test]
    fn a_path_with_special_chars_bakes_as_a_valid_escaped_literal() {
        let out = render_plugin(r#"/weird/pi"x\hook"#).unwrap();
        assert!(out.contains(r#""/weird/pi\"x\\hook""#));
    }

    #[test]
    fn uninstall_replaces_our_plugin_with_a_sentinel_free_stub() {
        let installed = render_plugin("/opt/bin/pixtuoid-hook").unwrap();
        let removed = uninstall(&installed).unwrap();
        assert!(removed.changed);
        assert!(
            !removed.content.contains(SENTINEL),
            "stub must drop the sentinel so detection flips"
        );
        assert_eq!(removed.content, REMOVED_STUB);
    }

    /// Whether `content` default-exports an `{ id, setup }` object, one of the
    /// two shapes opencode loads (`{ id, effect }` is the other,
    /// `packages/core/src/plugin/module.ts:78-91`).
    fn exports_a_plugin(content: &str) -> bool {
        content
            .split_once("export default {")
            .is_some_and(|(_, rest)| rest.contains("id:") && rest.contains("setup("))
    }

    /// The stub is still loaded as a plugin, so it must be a valid one.
    #[test]
    fn the_removed_stub_and_the_render_are_loadable_plugins() {
        assert!(exports_a_plugin(REMOVED_STUB), "{REMOVED_STUB}");
        assert!(exports_a_plugin(
            &render_plugin("/opt/bin/pixtuoid-hook").unwrap()
        ));
        assert!(!exports_a_plugin(V1_REMOVED_STUB));
    }

    #[test]
    fn uninstall_of_a_foreign_or_removed_file_is_a_no_op() {
        let foreign = "export const myPlugin = async () => ({})\n";
        assert!(!uninstall(foreign).unwrap().changed);
        assert!(!uninstall(REMOVED_STUB).unwrap().changed);
        assert!(!uninstall("").unwrap().changed);
        // `changed` is the connected fact (`install::has_hooks`), so the stub an
        // older pixtuoid's disconnect wrote must stay disconnected.
        assert!(!uninstall(V1_REMOVED_STUB).unwrap().changed);
    }

    #[test]
    fn install_then_uninstall_round_trips_the_content_sentinel() {
        let installed = render_plugin("/opt/bin/pixtuoid-hook").unwrap();
        assert!(installed.contains(SENTINEL));
        let removed = uninstall(&installed).unwrap();
        assert!(!removed.content.contains(SENTINEL));
    }

    #[test]
    fn config_dir_precedence_is_env_then_xdg_then_home() {
        assert_eq!(
            config_dir_from(
                Some(Path::new("/custom/oc")),
                Some(Path::new("/xdg")),
                Some(Path::new("/home/u"))
            )
            .unwrap(),
            PathBuf::from("/custom/oc")
        );
        assert_eq!(
            config_dir_from(None, Some(Path::new("/xdg")), Some(Path::new("/home/u"))).unwrap(),
            PathBuf::from("/xdg/opencode")
        );
        assert_eq!(
            config_dir_from(None, None, Some(Path::new("/home/u"))).unwrap(),
            PathBuf::from("/home/u/.config/opencode")
        );
        // Empty and whitespace-only env values are unset — enforced at the READ
        // (`platform::path_env`), which is where that policy now lives.
        // No home anywhere → a hard error (never a CWD-relative file).
        assert!(config_dir_from(None, None, None).is_err());
    }

    #[test]
    fn default_path_is_the_plugin_file_under_the_plural_plugins_dir() {
        assert_eq!(
            config_dir_from(None, Some(Path::new("/xdg")), None)
                .unwrap()
                .join("plugins")
                .join("pixtuoid.ts"),
            PathBuf::from("/xdg/opencode/plugins/pixtuoid.ts")
        );
    }

    #[test]
    fn hook_command_returns_the_absolute_path() {
        assert_eq!(
            hook_command(Path::new("/opt/bin/pixtuoid-hook"), false).unwrap(),
            "/opt/bin/pixtuoid-hook"
        );
    }

    #[test]
    #[cfg(unix)]
    fn hook_command_errors_on_non_utf8_path() {
        use std::os::unix::ffi::OsStrExt;
        let bad = Path::new(std::ffi::OsStr::from_bytes(b"/x/\xff/pixtuoid-hook"));
        assert!(hook_command(bad, false).is_err());
    }

    /// The binding test above catches a one-sided drop; this pin is what stops
    /// a SYMMETRIC one — plugin and decoder deleted together — from shipping
    /// silently.
    #[test]
    fn opencode_plugin_forward_set_is_pinned() {
        assert_eq!(
            plugin_events(),
            [
                "permission.asked",
                "session.created",
                "session.deleted",
                "session.execution.failed",
                "session.execution.interrupted",
                "session.execution.started",
                "session.execution.succeeded",
                "session.tool.failed",
                "session.tool.input.started",
                "session.tool.success",
            ],
            "opencode_plugin.ts's forwarded events changed — update the decoder and this pin together"
        );
    }

    /// Every event the PLUGIN forwards must decode — driven off the template's own
    /// set, so a new `FORWARD` entry with no decoder arm fails here rather than
    /// silently arriving as an unmapped event.
    #[test]
    fn every_forwarded_opencode_event_decodes() {
        use pixtuoid_core::source::decoder::decode_hook_payload;
        let payloads = plugin_events().into_iter().map(|ty| {
            serde_json::json!({"type": ty, "_pixtuoid_source": "opencode",
                "data": {"sessionID": "ses_1", "location": {"directory": "/r"},
                    "id": "c", "name": "bash", "action": "bash"}})
        });
        for p in payloads {
            let ty = p["type"].clone();
            assert!(
                decode_hook_payload(p).is_ok_and(|evs| !evs.is_empty()),
                "forwarded opencode event {ty} decoded to nothing — add an arm in source/opencode.rs"
            );
        }
    }
}
