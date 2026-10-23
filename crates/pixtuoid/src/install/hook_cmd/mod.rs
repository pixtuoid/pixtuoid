//! Hook-`command` quoting for CLIs whose hook runner SHELLS the command
//! (`/bin/sh -c` on Unix, `cmd.exe /C` on Windows), plus the argv-exec variant
//! for runners that word-split and exec directly.

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
/// and the READERS (`verify::shell_shim_ref`, `hermes::exec_shim_ref`). Both
/// surrounding spaces are part of the token, so every site splices/splits on the
/// exact same bytes and a rename can't produce a compiler-invisible write↔read
/// desync (a path with the un-stripped tail baked in reads as "shim missing").
/// The pixtuoid-hook crate parses the BARE argv tokens, a copy
/// `the_shim_spells_every_flag_and_stamp_as_its_writers_do` pins.
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

/// The OS-correct hook `command` for a CLI that ARGV-EXECs the command
/// (quote-aware word-split, NO shell). The env-prefix form would be treated as a
/// program literally named `PIXTUOID_SOURCE=<source>`, so the source rides as the
/// shim's `--source` FLAG instead.
///
/// - **Unix**: `'<path>' --source <source>` — the path single-quoted (Hermes
///   honors POSIX quotes, so a spaced path stays one token).
/// - **Windows**: BARE `<path> --source <source>`. Hermes's Windows arg-splitting
///   is unverified; the bare form is the safest for any splitter and mirrors the
///   Codex/Reasonix Windows form.
///
/// `source` is a registered source name: `every_registered_source_name_is_a_plain_identifier`
/// pins it shell/cmd-safe, so it splices unquoted.
pub(crate) fn exec_hook_command(path: &str, source: &str) -> Result<String> {
    #[cfg(windows)]
    {
        windows::windows_bare_hook_command(path, source)
    }
    #[cfg(unix)]
    {
        Ok(format!(
            "{}{SOURCE_FLAG}{source}",
            unix::shell_single_quote(path)
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn exec_form_quotes_path_and_appends_source_flag() {
        assert_eq!(
            exec_hook_command("/opt/bin/pixtuoid-hook", "hermes").unwrap(),
            "'/opt/bin/pixtuoid-hook' --source hermes"
        );
        assert_eq!(
            exec_hook_command("/Users/Jane Doe/bin/pixtuoid-hook", "hermes").unwrap(),
            "'/Users/Jane Doe/bin/pixtuoid-hook' --source hermes"
        );
    }

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

    #[test]
    fn the_shim_spells_every_flag_and_stamp_as_its_writers_do() {
        let shim = include_str!("../../../../pixtuoid-hook/src/main.rs");
        for flag in [SOURCE_FLAG, EVENT_FLAG, HOOK_EVENT_FLAG] {
            let bare = format!("\"{}\"", flag.trim());
            assert!(shim.contains(&bare), "the shim parses no {bare}");
        }
        let stamp = format!("\"{}\"", pixtuoid_core::source::decoder::EVENT_STAMP);
        assert!(shim.contains(&stamp), "the shim stamps no {stamp}");
    }
}
