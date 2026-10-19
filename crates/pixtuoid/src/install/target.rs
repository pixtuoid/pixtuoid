use std::path::{Path, PathBuf};

use anyhow::Result;

/// Each file pixtuoid owns beside a config, at its absolute path with `hook_cmd` baked
/// in, the one that bakes the shim first. The paths come from the CLI's OWN state
/// resolver, not the config path a test isolates itself with, so a test must redirect
/// that resolver too or it writes the developer's real plugin.
pub(crate) type RenderBeside = fn(hook_cmd: &str) -> Result<Vec<(PathBuf, String)>>;

pub(crate) type HookCommand = fn(resolved: &Path, explicit: bool) -> Result<String>;

/// Where the shim a rendered plugin file bakes lives, read back from its content.
pub(crate) type InstalledShim = fn(content: &str) -> crate::install::verify::ShimRef;

/// A target's owned files, whichever kind of primary keys them: all `install::converge_files`
/// is handed, so it cannot reach a config merge or a host registration.
#[derive(Debug, Clone, Copy)]
pub(crate) enum OwnedFiles {
    Primary {
        render: fn(hook_cmd: &str) -> Result<String>,
        installed_shim: InstalledShim,
    },
    /// Its first file bakes the shim the way [`crate::install::verify::baked_shim`]
    /// reads it, the reader doctor's `check_beside_shim` shares.
    Beside(RenderBeside),
}

impl OwnedFiles {
    /// Each file at its absolute path with `hook_cmd` baked in, the one that bakes the
    /// shim first; an owned primary sits at `primary`.
    pub(crate) fn render(self, primary: &Path, hook_cmd: &str) -> Result<Vec<(PathBuf, String)>> {
        match self {
            OwnedFiles::Primary { render, .. } => {
                Ok(vec![(primary.to_path_buf(), render(hook_cmd)?)])
            }
            OwnedFiles::Beside(render) => render(hook_cmd),
        }
    }

    pub(crate) fn installed_shim(self, content: &str) -> crate::install::verify::ShimRef {
        match self {
            OwnedFiles::Primary { installed_shim, .. } => installed_shim(content),
            OwnedFiles::Beside(_) => crate::install::verify::baked_shim(content),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryStrategy {
    /// Write the BARE name and rely on PATH. An unresolvable binary is non-fatal
    /// (falls back to the bare name); warn if not on PATH.
    BareNameOnPath,
    /// EMBED the resolved absolute path. An unresolvable binary is FATAL.
    EmbedAbsolute,
}

/// A merge's `changed` compares the PARSED document before and after, never the
/// serialized bytes (which always differ from a hand-formatted file): a byte
/// comparison would make a semantic no-op look like a change, rewriting the user's
/// formatting. An owned primary is pixtuoid's whole file, so its install compares bytes.
#[derive(Debug)]
pub struct MergeOutcome {
    pub content: String,
    pub changed: bool,
}

/// A single install destination (one CLI's primary file). `const` data, not
/// `static`: `&CONST` in `const TARGETS` is legal via rvalue static promotion.
#[derive(Debug)]
pub struct Target {
    /// The `--target` / CLI-facing id, which does NOT always equal the core
    /// source id — join against the source registry via `core_source`, never this.
    pub name: &'static str,
    /// The core `SourceDescriptor.name` this target installs hooks FOR — usually
    /// `name`, but Claude's target is "claude" while its source is "claude-code".
    /// The Sources panel joins its rows to targets on this via `by_source`.
    pub core_source: &'static str,
    pub display_name: &'static str,
    /// The file the target is keyed on ([`Primary`] says whose; reads $HOME, hence a
    /// fn not a const). Errs when no home dir resolves — a CWD fallback would
    /// "succeed" with a file the CLI never reads.
    pub default_config_path: fn() -> Result<PathBuf>,
    /// Build the command string written into config from the resolved binary.
    /// `explicit` (the user set
    /// [`HOOK_OVERRIDE_ENV`](crate::install::io::HOOK_OVERRIDE_ENV)) always wins
    /// over a bare PATH-resolved name. Usually this IS the verbatim command written for every
    /// event, but a Config target's `merge_install` MAY append a per-entry suffix —
    /// CodeWhale bakes ` --event <name>` onto each entry, so its `hook_command`
    /// returns a per-source BASE that `merge_install` extends.
    pub hook_command: HookCommand,
    pub primary: Primary,
    pub binary_strategy: BinaryStrategy,
    /// Whether the CLI is installed, read from something the CLI itself creates: the
    /// file pixtuoid writes would mean auto-detection never fires before a connect.
    pub presence_probe: fn() -> bool,
    /// A one-line note the presenters append after a SUCCESSFUL connect, when
    /// installing is not the last step the user must take. `None` for every target
    /// whose hooks take effect on the CLI's next run.
    pub post_install_hint: Option<&'static str>,
    /// Set when the CLI loads our hooks through its own plugin system: the owned
    /// primary is then the plugin file that carries them (a hooks file, or a
    /// manifest with the hooks inside), which this registers with the CLI.
    pub host: Option<&'static HostRegistration>,
    /// The hook names the installed artifact subscribes to: a profiled CLI's
    /// hermetic scenarios fire or owe each it forwards
    /// (`every_hook_a_profiled_cli_forwards_is_fired_or_owed`).
    #[cfg(test)]
    pub(crate) registers: fn() -> Vec<&'static str>,
    /// Those the artifact consumes itself and never forwards, so no capture
    /// holds them.
    #[cfg(test)]
    pub(crate) consumes: &'static [&'static str],
}

/// Whose the primary file is: the user's, with pixtuoid's entries merged in, or
/// pixtuoid's whole. Exactly one per target, so no target is both or neither.
#[derive(Debug)]
pub enum Primary {
    Config {
        /// Parse `content`, inject managed hook entries, reserialize. MUST treat
        /// empty/whitespace-only content as the empty document — never error on empty.
        merge_install: fn(content: &str, hook_cmd: &str) -> Result<MergeOutcome>,
        /// Remove only managed entries. Same empty rule.
        merge_uninstall: fn(content: &str) -> Result<MergeOutcome>,
        /// Verify the installed config is structurally sound — PURE over the config
        /// content, extracting the shim path for `install::verify_target` to stat.
        /// Per-source format knowledge stays here, like the merge fns.
        verify_schema: fn(content: &str) -> crate::install::verify::SchemaParse,
        /// Files pixtuoid owns beside the config, where [`RenderBeside`] says they
        /// live. Left in place on uninstall (the config un-merge disables loading).
        beside: Option<RenderBeside>,
    },
    Owned {
        /// The primary's whole content, `hook_cmd` baked in.
        render: fn(hook_cmd: &str) -> Result<String>,
        installed_shim: InstalledShim,
        /// The primary as an uninstall leaves it; `changed` iff it was ours. Same
        /// empty rule as `merge_uninstall`.
        uninstall: fn(content: &str) -> Result<MergeOutcome>,
        /// As `Config`'s, over the owned primary.
        verify_schema: fn(content: &str) -> crate::install::verify::SchemaParse,
        retired: Option<RetiredStub>,
        /// Whether the CLI reloads the primary when it changes, so a rewrite
        /// needs no restart notice.
        reloads_on_change: bool,
    },
}

/// A stub an older pixtuoid's uninstall wrote that the CLI no longer loads,
/// and its successor. `uninstall`'s `changed` is the connected fact, so `was`
/// must never read as ours there.
#[derive(Debug)]
pub struct RetiredStub {
    pub was: &'static str,
    pub now: &'static str,
}

impl Primary {
    /// What an uninstall does to the primary's content, and the dry run `has_hooks` reads.
    pub(crate) fn uninstall(&self) -> fn(&str) -> Result<MergeOutcome> {
        match self {
            Primary::Config {
                merge_uninstall, ..
            } => *merge_uninstall,
            Primary::Owned { uninstall, .. } => *uninstall,
        }
    }

    pub(crate) fn verify(&self) -> fn(&str) -> crate::install::verify::SchemaParse {
        match self {
            Primary::Config { verify_schema, .. } | Primary::Owned { verify_schema, .. } => {
                *verify_schema
            }
        }
    }

    pub(crate) fn reloads_on_change(&self) -> bool {
        matches!(
            self,
            Primary::Owned {
                reloads_on_change: true,
                ..
            }
        )
    }

    pub(crate) fn owned_files(&self) -> Option<OwnedFiles> {
        match *self {
            Primary::Config { beside, .. } => beside.map(OwnedFiles::Beside),
            Primary::Owned {
                render,
                installed_shim,
                ..
            } => Some(OwnedFiles::Primary {
                render,
                installed_shim,
            }),
        }
    }
}

/// A CLI-side plugin registration, run through the CLI's own commands so the CLI
/// owns its settings.
#[derive(Debug)]
pub struct HostRegistration {
    pub(crate) cli: crate::install::io::HostCli,
    /// Register the plugin around the owned primary `config`, first writing any
    /// manifest the CLI needs beside it. Idempotent, and run on every install so a
    /// deregistered plugin heals.
    pub register: fn(config: &Path) -> Result<()>,
    /// Deregister the plugin and delete its files, `config` included; an
    /// unreachable CLI leaves both in place.
    pub unregister: fn(config: &Path) -> Result<Unregistered>,
    /// Whether the CLI reports the plugin installed and enabled.
    pub is_registered: fn() -> Result<bool>,
    /// `None` when the CLI loads the plugin's files in place.
    pub copy: Option<PluginCopy>,
    /// `None` when no released pixtuoid ever merged hooks into the CLI's own config.
    pub legacy: Option<LegacyConfig>,
}

/// A CLI that runs its own copy of the plugin, so a rewrite of the files reaches it
/// only once re-copied.
#[derive(Debug)]
pub struct PluginCopy {
    /// Whether the copy differs from the plugin around the owned primary `config`.
    pub stale: fn(config: &Path) -> Result<bool>,
    /// Re-copy the plugin. It may also re-enable a plugin the user disabled, so it
    /// runs only while [`HostRegistration::is_registered`].
    pub refresh: fn() -> Result<()>,
}

/// A config of the CLI's own that an older pixtuoid merged its hooks into. Connect
/// and disconnect both strip them, so no event fires twice.
#[derive(Debug)]
pub struct LegacyConfig {
    pub path: fn() -> Result<PathBuf>,
    /// Remove only the entries pixtuoid merged, as `Primary::Config`'s `merge_uninstall`.
    pub strip: fn(content: &str) -> Result<MergeOutcome>,
}

/// What [`HostRegistration::unregister`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unregistered {
    /// The CLI had the plugin, and it is gone.
    Removed,
    /// The CLI had nothing to deregister.
    Absent,
    /// The CLI couldn't be reached, so the plugin stays registered.
    Unreachable,
}

pub(crate) const CLAUDE: Target = Target {
    name: "claude",
    core_source: pixtuoid_core::source::claude_code::SOURCE_NAME,
    display_name: "Claude Code",
    default_config_path: crate::install::claude::default_config_path,
    hook_command: crate::install::claude::hook_command,
    primary: Primary::Owned {
        render: crate::install::claude::render_hooks,
        installed_shim: crate::install::claude::installed_shim,
        uninstall: crate::install::host_plugin::uninstall_hooks,
        verify_schema: crate::install::claude::verify_schema,
        retired: None,
        reloads_on_change: false,
    },
    // Windows: the exec form embeds the absolute path, and a hook spawned without
    // a shell can't PATH-search, so an unresolvable binary is fatal there.
    binary_strategy: if cfg!(windows) {
        BinaryStrategy::EmbedAbsolute
    } else {
        BinaryStrategy::BareNameOnPath
    },
    presence_probe: crate::install::claude::detect_installed,
    post_install_hint: None,
    host: Some(&crate::install::claude::HOST),
    #[cfg(test)]
    consumes: &[],
    #[cfg(test)]
    registers: || crate::install::claude::EVENTS.to_vec(),
};

pub(crate) const CODEX: Target = Target {
    name: "codex",
    core_source: pixtuoid_core::source::codex::SOURCE_NAME,
    display_name: "Codex",
    default_config_path: crate::install::codex::default_config_path,
    hook_command: crate::install::codex::hook_command,
    primary: Primary::Owned {
        render: crate::install::codex::render_hooks,
        installed_shim: crate::install::codex::installed_shim,
        uninstall: crate::install::host_plugin::uninstall_hooks,
        verify_schema: crate::install::codex::verify_schema,
        retired: None,
        reloads_on_change: false,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    presence_probe: crate::install::codex::detect_installed,
    // Untrusted hooks never run (`codex-rs/tui/src/startup_hooks_review.rs`
    // @ rust-v0.162.1), and a changed hook asks again.
    post_install_hint: Some(
        "restart Codex and trust pixtuoid's hooks at its \"Hooks need review\" prompt",
    ),
    host: Some(&crate::install::codex::HOST),
    #[cfg(test)]
    consumes: &[],
    #[cfg(test)]
    registers: || crate::install::codex::CODEX_EVENTS.to_vec(),
};

pub(crate) const REASONIX: Target = Target {
    name: "reasonix",
    core_source: pixtuoid_core::source::reasonix::SOURCE_NAME,
    display_name: "Reasonix",
    default_config_path: crate::install::reasonix::default_config_path,
    hook_command: crate::install::reasonix::hook_command,
    primary: Primary::Owned {
        render: crate::install::reasonix::render_manifest,
        installed_shim: crate::install::reasonix::installed_shim,
        uninstall: crate::install::reasonix::uninstall,
        verify_schema: crate::install::reasonix::verify_schema,
        retired: None,
        reloads_on_change: false,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    presence_probe: crate::install::reasonix::detect_installed,
    post_install_hint: None,
    host: Some(&crate::install::reasonix::HOST),
    #[cfg(test)]
    consumes: &[],
    #[cfg(test)]
    registers: || crate::install::reasonix::REASONIX_EVENTS.to_vec(),
};

pub(crate) const CODEWHALE: Target = Target {
    name: "codewhale",
    core_source: pixtuoid_core::source::codewhale::SOURCE_NAME,
    display_name: "CodeWhale",
    default_config_path: crate::install::codewhale::default_config_path,
    hook_command: crate::install::codewhale::hook_command,
    primary: Primary::Config {
        merge_install: crate::install::codewhale::merge_install,
        merge_uninstall: crate::install::codewhale::merge_uninstall,
        verify_schema: crate::install::codewhale::verify_schema,
        beside: None,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    presence_probe: crate::install::codewhale::detect_installed,
    post_install_hint: None,
    host: None,
    #[cfg(test)]
    consumes: &[],
    #[cfg(test)]
    registers: || {
        crate::install::codewhale::CODEWHALE_EVENTS
            .iter()
            .map(|&(name, _)| name)
            .collect()
    },
};

pub(crate) const OPENCODE: Target = Target {
    name: "opencode",
    core_source: pixtuoid_core::source::opencode::SOURCE_NAME,
    display_name: "opencode",
    default_config_path: crate::install::opencode::default_config_path,
    hook_command: crate::install::opencode::hook_command,
    primary: Primary::Owned {
        render: crate::install::opencode::render_plugin,
        installed_shim: crate::install::verify::baked_shim,
        uninstall: crate::install::opencode::uninstall,
        verify_schema: crate::install::opencode::verify_schema,
        retired: Some(RetiredStub {
            was: crate::install::opencode::V1_REMOVED_STUB,
            now: crate::install::opencode::REMOVED_STUB,
        }),
        // `packages/core/src/plugin/supervisor.ts:219` watches `plugins/`.
        reloads_on_change: true,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    // A post-uninstall stub still exists, so detect on opencode's own dirs
    // rather than our plugin file.
    presence_probe: crate::install::opencode::detect_installed,
    post_install_hint: None,
    host: None,
    #[cfg(test)]
    consumes: &[],
    #[cfg(test)]
    registers: crate::install::opencode::plugin_events,
};

pub(crate) const GROK: Target = Target {
    name: "grok",
    core_source: pixtuoid_core::source::grok::SOURCE_NAME,
    display_name: "Grok Build",
    default_config_path: crate::install::grok::default_config_path,
    hook_command: crate::install::grok::hook_command,
    primary: Primary::Config {
        merge_install: crate::install::grok::merge_install,
        merge_uninstall: crate::install::grok::merge_uninstall,
        verify_schema: crate::install::grok::verify_schema,
        beside: None,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    presence_probe: crate::install::grok::detect_installed,
    post_install_hint: None,
    host: None,
    #[cfg(test)]
    consumes: &[],
    #[cfg(test)]
    registers: || crate::install::grok::GROK_EVENTS.to_vec(),
};

pub(crate) const CURSOR: Target = Target {
    name: "cursor",
    core_source: pixtuoid_core::source::cursor::SOURCE_NAME,
    display_name: "Cursor CLI",
    default_config_path: crate::install::cursor::default_config_path,
    hook_command: crate::install::cursor::hook_command,
    primary: Primary::Config {
        merge_install: crate::install::cursor::merge_install,
        merge_uninstall: crate::install::cursor::merge_uninstall,
        verify_schema: crate::install::cursor::verify_schema,
        beside: None,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    presence_probe: crate::install::cursor::detect_installed,
    post_install_hint: None,
    host: None,
    #[cfg(test)]
    consumes: &[],
    #[cfg(test)]
    registers: || crate::install::cursor::CURSOR_EVENTS.to_vec(),
};

pub(crate) const HERMES: Target = Target {
    name: "hermes",
    core_source: pixtuoid_core::source::hermes::SOURCE_NAME,
    display_name: "Hermes Agent",
    default_config_path: crate::install::hermes::default_config_path,
    hook_command: crate::install::hermes::hook_command,
    primary: Primary::Config {
        merge_install: crate::install::hermes::merge_install,
        merge_uninstall: crate::install::hermes::merge_uninstall,
        verify_schema: crate::install::hermes::verify_schema,
        beside: None,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    // Hermes OWNS config.yaml (model/provider), so probe the home dir instead.
    presence_probe: crate::install::hermes::detect_installed,
    post_install_hint: None,
    host: None,
    #[cfg(test)]
    consumes: &[],
    #[cfg(test)]
    registers: || crate::install::hermes::HERMES_EVENTS.to_vec(),
};

pub(crate) const OPENCLAW: Target = Target {
    name: "openclaw",
    core_source: pixtuoid_core::source::openclaw::SOURCE_NAME,
    display_name: "OpenClaw",
    default_config_path: crate::install::openclaw::default_config_path,
    hook_command: crate::install::openclaw::hook_command,
    primary: Primary::Config {
        merge_install: crate::install::openclaw::merge_install,
        merge_uninstall: crate::install::openclaw::merge_uninstall,
        verify_schema: crate::install::openclaw::verify_schema,
        beside: Some(crate::install::openclaw::plugin_artifacts),
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    // openclaw.json is created by OpenClaw itself; probe its own dir so
    // auto-detect fires whether or not we've installed.
    presence_probe: crate::install::openclaw::detect_installed,
    // `plugins.load` is `kind: "restart"` upstream, so a RUNNING gateway does not
    // hot-load the plugin — the lobster appears on its next restart.
    post_install_hint: Some("restart the gateway to load it (openclaw gateway restart)"),
    host: None,
    #[cfg(test)]
    registers: || {
        [
            crate::install::openclaw::OPENCLAW_EVENTS,
            crate::install::openclaw::OPENCLAW_VERDICT_EVENTS,
        ]
        .concat()
    },
    #[cfg(test)]
    consumes: crate::install::openclaw::OPENCLAW_VERDICT_EVENTS,
};

pub(crate) const KIMI: Target = Target {
    name: "kimi",
    core_source: pixtuoid_core::source::kimi::SOURCE_NAME,
    display_name: "Kimi Code CLI",
    default_config_path: crate::install::kimi::default_config_path,
    hook_command: crate::install::kimi::hook_command,
    primary: Primary::Config {
        merge_install: crate::install::kimi::merge_install,
        merge_uninstall: crate::install::kimi::merge_uninstall,
        verify_schema: crate::install::kimi::verify_schema,
        beside: None,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    // Kimi may not create config.toml until a provider is configured, so probe the
    // data root rather than the file we write.
    presence_probe: crate::install::kimi::detect_installed,
    post_install_hint: None,
    host: None,
    #[cfg(test)]
    consumes: &[],
    #[cfg(test)]
    registers: || crate::install::kimi::KIMI_EVENTS.to_vec(),
};

pub(crate) const DSH: Target = Target {
    name: "dsh",
    core_source: pixtuoid_core::source::dsh::SOURCE_NAME,
    display_name: "DeepSeek Harness",
    default_config_path: crate::install::dsh::default_config_path,
    hook_command: crate::install::dsh::hook_command,
    primary: Primary::Config {
        merge_install: crate::install::dsh::merge_install,
        merge_uninstall: crate::install::dsh::merge_uninstall,
        verify_schema: crate::install::dsh::verify_schema,
        beside: Some(crate::install::dsh::plugin_artifacts),
    },
    // The plugin spawns the shim under Node by embedded path — no PATH.
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    presence_probe: crate::install::dsh::detect_installed,
    // Profiles other than `web` compose patches once at boot (`web` watches
    // and hot-reloads both user patch files).
    post_install_hint: Some(
        "running dsh sessions (except the web profile) must restart to load the plugin",
    ),
    host: None,
    #[cfg(test)]
    consumes: &[],
    #[cfg(test)]
    registers: crate::install::dsh::plugin_events,
};

pub(crate) const OMP: Target = Target {
    name: "omp",
    core_source: pixtuoid_core::source::omp::SOURCE_NAME,
    display_name: "Oh My Pi",
    default_config_path: crate::install::omp::default_config_path,
    hook_command: crate::install::omp::hook_command,
    primary: Primary::Owned {
        render: crate::install::omp::render_extension,
        installed_shim: crate::install::verify::baked_shim,
        uninstall: crate::install::omp::uninstall,
        verify_schema: crate::install::omp::verify_schema,
        retired: None,
        reloads_on_change: false,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    // A post-uninstall stub still exists, so detect on omp's own agent dir
    // rather than our file.
    presence_probe: crate::install::omp::detect_installed,
    // Extensions load once at startup (`discoverAndLoadExtensions`).
    post_install_hint: Some("already-running omp sessions must restart to load the bridge"),
    host: None,
    #[cfg(test)]
    consumes: &[],
    #[cfg(test)]
    registers: || {
        crate::install::forward_set(crate::install::omp::EXTENSION_TEMPLATE)
            .into_iter()
            .collect()
    },
};

pub(crate) const ANTIGRAVITY: Target = Target {
    name: "antigravity",
    core_source: pixtuoid_core::source::antigravity::SOURCE_NAME,
    display_name: "Antigravity CLI",
    default_config_path: crate::install::antigravity::default_config_path,
    hook_command: crate::install::antigravity::hook_command,
    primary: Primary::Config {
        merge_install: crate::install::antigravity::merge_install,
        merge_uninstall: crate::install::antigravity::merge_uninstall,
        verify_schema: crate::install::antigravity::verify_schema,
        beside: None,
    },
    binary_strategy: BinaryStrategy::EmbedAbsolute,
    presence_probe: crate::install::antigravity::detect_installed,
    post_install_hint: None,
    host: None,
    #[cfg(test)]
    consumes: &[],
    #[cfg(test)]
    registers: || {
        crate::install::antigravity::AG_EVENTS
            .iter()
            .map(|&(name, _)| name)
            .collect()
    },
};

pub(crate) const TARGETS: &[&Target] = &[
    &CLAUDE,
    &CODEX,
    &REASONIX,
    &CODEWHALE,
    &OPENCODE,
    &CURSOR,
    &HERMES,
    &OPENCLAW,
    &GROK,
    &KIMI,
    &OMP,
    &DSH,
    &ANTIGRAVITY,
];

// No prod caller: prod joins on the source id via `by_source`. Kept for the
// target-registry tests; un-gate if prod ever needs it.
#[cfg(test)]
pub(crate) fn by_name(name: &str) -> Option<&'static Target> {
    TARGETS.iter().copied().find(|t| t.name == name)
}

/// The correct join for anything keyed on the source registry — `by_name` keys
/// on the CLI-facing `--target` name, which differs from the source id.
pub(crate) fn by_source(source_id: &str) -> Option<&'static Target> {
    TARGETS.iter().copied().find(|t| t.core_source == source_id)
}

/// Detection = the config FILE exists, not merely its parent dir: an empty
/// ~/.codex must NOT count as present.
pub(crate) fn config_present(path: &Path) -> bool {
    crate::install::io::resolve_symlink(path).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_resolves_claude_and_rejects_unknown() {
        assert_eq!(by_name("claude").unwrap().name, "claude");
        assert_eq!(by_name("codex").unwrap().name, "codex");
        assert_eq!(by_name("reasonix").unwrap().name, "reasonix");
        assert_eq!(by_name("codewhale").unwrap().name, "codewhale");
        assert_eq!(by_name("opencode").unwrap().name, "opencode");
        assert_eq!(by_name("cursor").unwrap().name, "cursor");
        assert_eq!(by_name("hermes").unwrap().name, "hermes");
        assert_eq!(by_name("openclaw").unwrap().name, "openclaw");
        assert_eq!(by_name("kimi").unwrap().name, "kimi");
        assert!(by_name("nope").is_none());
        assert!(by_name("all").is_none()); // "all" is a meta-value, not a Target
    }

    #[test]
    fn by_source_resolves_claude_via_core_source_not_name() {
        assert_eq!(by_source("claude-code").unwrap().name, "claude");
        assert!(by_name("claude-code").is_none());
        assert!(by_source("nope").is_none());
    }

    #[test]
    fn config_present_checks_file_existence() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("x.json");
        assert!(!config_present(&p));
        std::fs::write(&p, "{}").unwrap();
        assert!(config_present(&p));
    }

    #[test]
    fn every_target_names_a_registered_source() {
        use pixtuoid_core::source::registry;
        for t in TARGETS {
            assert!(
                registry::registered_source_names().any(|s| s == t.core_source),
                "install target {:?} names core_source {:?}, which is not a registered source \
                 (typo, or a renamed source) — fix the target or register the source",
                t.name,
                t.core_source
            );
        }
    }

    // Transcript-bearing sources may legitimately have no target, so the rule is
    // derived from `line_decoder().is_none()` — no exemption list to drift.
    #[test]
    fn every_hook_only_source_has_an_install_target() {
        use pixtuoid_core::source::registry::{self, descriptor_for};
        for src in registry::registered_source_names() {
            let d = descriptor_for(src).expect("registered source must have a descriptor row");
            if d.line_decoder().is_none() {
                assert!(
                    TARGETS.iter().any(|t| t.core_source == src),
                    "hook-only source {src:?} has no install target — its hooks would never \
                     install, so its sprite never appears. Add a Target in install/target.rs."
                );
            }
        }
    }

    #[test]
    fn every_hook_a_profiled_cli_forwards_is_fired_or_owed() {
        use std::collections::BTreeSet;
        for p in pixtuoid_core::harness::hermetic::PROFILES {
            // A CLI with no install target forwards no hook (copilot).
            let forwarded: BTreeSet<&str> = by_source(p.source)
                .map(|t| {
                    let mut hooks = (t.registers)();
                    hooks.retain(|h| !t.consumes.contains(h));
                    hooks
                })
                .unwrap_or_default()
                .into_iter()
                .collect();
            let accounted: BTreeSet<&str> = p
                .scenarios
                .iter()
                .flat_map(|s| s.expects.iter().copied())
                .chain(p.not_yet.iter().copied())
                .collect();
            assert_eq!(
                forwarded, accounted,
                "{}: a forwarded hook no scenario fires belongs in not_yet; \
                 an expected or not_yet hook the install never forwards is stale",
                p.source
            );
        }
    }

    #[test]
    fn target_core_sources_are_unique() {
        use std::collections::HashSet;
        let set: HashSet<&str> = TARGETS.iter().map(|t| t.core_source).collect();
        assert_eq!(
            set.len(),
            TARGETS.len(),
            "two install targets claim the same core_source — one source, double hooks"
        );
    }
}
