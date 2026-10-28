//! Hook-`command` quoting for CLIs whose hook runner SHELLS the command
//! (`/bin/sh -c` on Unix, `cmd.exe /C` on Windows).

use anyhow::Result;

#[cfg(unix)]
pub(crate) mod unix;
// Compiled on all platforms so the cmd-safety decision core (with the 8.3
// resolver injected) unit-tests on macOS; only the Win32 FFI is `#[cfg(windows)]`.
mod windows;

/// The DOS 8.3 short name for an existing path (metachar-free by construction) —
/// `None` when 8.3 generation is disabled on the volume. Also used by targets
/// whose hook runner is NOT cmd.exe: a short name needs no quoting under ANY
/// shell.
#[cfg(windows)]
pub(crate) fn windows_short_path(long: &str) -> Option<String> {
    windows::short_path_windows(long)
}

/// The shim's source/event flag tokens — the ONE spelling shared by the WRITERS
/// and the READER (`verify::shell_shim_ref`). Both
/// surrounding spaces are part of the token, so every site splices/splits on the
/// exact same bytes and a rename can't produce a compiler-invisible write↔read
/// desync (a path with the un-stripped tail baked in reads as "shim missing").
/// The pixtuoid-hook crate parses the BARE argv tokens, a copy
/// `the_shim_parses_every_flag_its_writers_spell` pins.
pub(crate) const SOURCE_FLAG: &str = " --source ";
pub(crate) const EVENT_FLAG: &str = " --event ";
pub(crate) const HOOK_EVENT_FLAG: &str = " --hook-event ";

/// The OS-correct hook `command` string for a shell-running CLI — the single OS
/// fork for that strategy.
///
/// - **Unix**: env-prefix form `PIXTUOID_SOURCE=<source> '<path>'`.
/// - **Windows**: BARE exec form `<path> --source <source>` — cmd.exe can't
///   express the env-prefix, so the source rides the shim's `--source` flag and a
///   space/metacharacter path falls back to its DOS 8.3 short name.
///
/// `source` is a registered source name: `every_registered_source_name_is_a_plain_identifier`
/// pins it shell/cmd-safe, so it splices unquoted.
pub(crate) fn shell_hook_command(path: &str, source: &str) -> Result<String> {
    #[cfg(windows)]
    {
        windows::windows_bare_hook_command(path, source)
    }
    #[cfg(unix)]
    {
        Ok(format!(
            "PIXTUOID_SOURCE={source} {}",
            unix::shell_single_quote(path)
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn valid_source_keeps_the_env_prefix_form_byte_for_byte() {
        assert_eq!(
            shell_hook_command("/opt/bin/pixtuoid-hook", "codex").unwrap(),
            "PIXTUOID_SOURCE=codex '/opt/bin/pixtuoid-hook'"
        );
        assert_eq!(
            shell_hook_command("/opt/bin/pixtuoid-hook", "claude-code").unwrap(),
            "PIXTUOID_SOURCE=claude-code '/opt/bin/pixtuoid-hook'"
        );
    }
}

#[cfg(test)]
mod parity {
    use super::{EVENT_FLAG, HOOK_EVENT_FLAG, SOURCE_FLAG};

    /// The hook writers splice the source name unquoted into a shell or cmd.exe
    /// command line.
    #[test]
    fn every_registered_source_name_is_a_plain_identifier() {
        for name in pixtuoid_core::source::registry::registered_source_names() {
            assert!(
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
                "{name:?} is no safe command-line token"
            );
        }
    }

    /// Its stamps are pinned by `each_key_the_shim_stamps_is_the_one_core_reads`.
    #[test]
    fn the_shim_parses_every_flag_its_writers_spell() {
        let shim = include_str!("../../../../pixtuoid-hook/src/main.rs");
        for flag in [SOURCE_FLAG, EVENT_FLAG, HOOK_EVENT_FLAG] {
            let bare = format!("\"{}\"", flag.trim());
            assert!(shim.contains(&bare), "the shim parses no {bare}");
        }
    }
}
