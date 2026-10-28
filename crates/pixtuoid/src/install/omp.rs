//! omp bridge install target — a TS EXTENSION, not a config block (#951).
//!
//! omp has no config-level shell hook (its hooks are in-process TS extension
//! modules), so pixtuoid drops an extension at
//! `<omp-agent-dir>/extensions/pixtuoid.ts`, which omp auto-discovers at
//! startup (`--no-extensions` opts out and leaves the transcript-only path).
//! The extension forwards the lifecycle/approval allowlist into the
//! `pixtuoid-hook` shim; the shim's absolute path is baked in (JSON-escaped)
//! at install time from the `omp_extension.ts` template.
//!
//! The extension FILE is the target's owned primary, so install renders the
//! whole file and a disconnect replaces it with a sentinel-free no-op stub.
//! ACCEPTED residual: uninstall leaves that stub rather than deleting the file
//! — the orchestrator's `write_atomic` can't delete, and the stub is a harmless
//! empty module.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::install::merge::SentinelPlugin;
use crate::install::target::MergeOutcome;

/// Marks the extension as ours — absent from the removed-stub, so an uninstall
/// of a foreign/removed file is a clean no-op.
const SENTINEL: &str = "@pixtuoid-omp-extension";

pub(crate) const EXTENSION_TEMPLATE: &str = include_str!("omp_extension.ts");

/// A valid empty ES module WITHOUT the sentinel, so a re-uninstall is a no-op.
const REMOVED_STUB: &str = "// pixtuoid omp extension removed by disconnecting omp in pixtuoid's Sources panel (press s).\nexport default function () {}\n";

const EXTENSION: SentinelPlugin = SentinelPlugin {
    sentinel: SENTINEL,
    removed_stub: REMOVED_STUB,
    what: "the omp extension",
    cli: "omp",
};

/// The extensions root is core's [`omp_extensions_dir`], never a second copy of
/// that resolver (#880). A RELATIVE `PI_CODING_AGENT_DIR` is refused rather
/// than resolved: upstream resolves it against OMP's own cwd, which pixtuoid
/// cannot know, so any absolutization here would be a confidently wrong write.
/// Accepted residual: omp discovers extensions through a gitignore-filtered
/// glob, so a dotfiles-repo ignore rule covering this path can suppress loading
/// while every check here reads green.
///
/// [`omp_extensions_dir`]: pixtuoid_core::source::omp::omp_extensions_dir
pub(crate) fn default_config_path() -> Result<PathBuf> {
    let dir = pixtuoid_core::source::omp::omp_extensions_dir();
    if !dir.is_absolute() {
        bail!(
            "omp's agent dir resolved to the relative path {} — PI_CODING_AGENT_DIR \
             must be an absolute path for pixtuoid to install the bridge",
            dir.display()
        );
    }
    Ok(dir.join("pixtuoid.ts"))
}

/// Presence probe for auto-detect: probe omp's OWN agent dir, NOT our
/// extension file — keying on our own artifact would chicken-and-egg (omp
/// could never be auto-detected until AFTER we'd installed into it).
pub(crate) fn detect_installed() -> bool {
    pixtuoid_core::source::omp::omp_agent_dir().exists()
}

/// omp runs extensions under Bun and spawns the shim by embedded path (no
/// PATH reliance), so `_explicit` — Claude's bare-vs-absolute switch — is
/// irrelevant here: omp always needs the absolute path.
pub(crate) fn hook_command(resolved: &Path, _explicit: bool) -> Result<String> {
    crate::install::merge::hook_path_str(resolved).map(str::to_string)
}

pub(crate) fn uninstall(content: &str) -> Result<MergeOutcome> {
    Ok(EXTENSION.uninstall(content))
}

pub(crate) fn verify_schema(content: &str) -> crate::install::verify::SchemaParse {
    use crate::install::verify::{baked_shim, sentinel_plugin_schema};
    sentinel_plugin_schema(&EXTENSION, content, baked_shim(content))
}

pub(crate) fn render_extension(hook_path: &str) -> Result<String> {
    crate::install::merge::bake_hook_path(EXTENSION_TEMPLATE, hook_path, "omp")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::merge::HOOK_PLACEHOLDER;

    /// The bridge must never hold a gate: omp's `tool_call` handlers fail
    /// CLOSED (a throw/timeout blocks the user's tool), so the extension may
    /// only ever observe, and its selftest holds what it registers to
    /// `DECODED_HOOK_EVENTS`.
    #[test]
    fn the_extension_registers_no_gating_handlers_and_awaits_nothing() {
        // The one awaited thing is the shim, bounded by its own
        // `transport::WRITE_TIMEOUT_MS` watchdog.
        assert_eq!(
            EXTENSION_TEMPLATE.matches("await proc.exited").count(),
            1,
            "exactly one await point — the bounded shim"
        );
        assert!(
            !EXTENSION_TEMPLATE
                .replace("await proc.exited", "")
                .contains("await "),
            "anything else awaited can hold omp's handler budget"
        );
        for gate in ["tool_call", "tool_result", "input", "session_before"] {
            assert!(
                !pixtuoid_core::source::omp::DECODED_HOOK_EVENTS
                    .iter()
                    .any(|e| e.starts_with(gate)),
                "`{gate}*` is a blocking/gating handler class — the bridge is \
                 observe-only"
            );
        }
    }

    #[test]
    fn verify_schema_reports_the_baked_shim_and_every_way_the_bridge_can_be_dead() {
        use crate::install::verify::ShimRef;

        let installed = render_extension("/opt/bin/pixtuoid-hook").unwrap();
        let sound = verify_schema(&installed);
        assert!(
            sound.issues.is_empty(),
            "a freshly rendered extension is sound — got {:?}",
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
            foreign.issues.iter().any(|i| i.contains("reconnect omp")),
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
        let out = render_extension("/opt/bin/pixtuoid-hook").unwrap();
        assert!(out.contains(SENTINEL));
        assert!(out.contains("\"/opt/bin/pixtuoid-hook\""));
        assert!(!out.contains(HOOK_PLACEHOLDER));
        assert!(
            out.contains("--source"),
            "spawns the shim with --source omp"
        );
    }

    #[test]
    fn a_path_with_special_chars_bakes_as_a_valid_escaped_literal() {
        let out = render_extension(r#"/weird/pi"x\hook"#).unwrap();
        assert!(out.contains(r#""/weird/pi\"x\\hook""#));
    }

    #[test]
    fn uninstall_replaces_our_extension_with_a_sentinel_free_stub() {
        let installed = render_extension("/opt/bin/pixtuoid-hook").unwrap();
        let removed = uninstall(&installed).unwrap();
        assert!(removed.changed);
        assert!(
            !removed.content.contains(SENTINEL),
            "stub must drop the sentinel so detection flips"
        );
        assert!(
            removed.content.contains("export default function () {}"),
            "stub is a valid inert module — omp still imports and calls it"
        );
    }

    #[test]
    fn uninstall_of_a_foreign_or_removed_file_is_a_no_op() {
        let foreign = "export default function (pi) {}\n";
        assert!(!uninstall(foreign).unwrap().changed);
        assert!(!uninstall(REMOVED_STUB).unwrap().changed);
        assert!(!uninstall("").unwrap().changed);
    }

    /// The one string deciding whether omp ever LOADS what install writes.
    #[test]
    fn default_path_is_the_extension_under_the_agent_dirs_extensions() {
        let p = default_config_path().expect("resolvable in a dev environment");
        assert!(p.ends_with("extensions/pixtuoid.ts"), "got {}", p.display());
    }

    #[test]
    fn hook_command_returns_the_absolute_path() {
        assert_eq!(
            hook_command(Path::new("/opt/bin/pixtuoid-hook"), false).unwrap(),
            "/opt/bin/pixtuoid-hook"
        );
    }
}
