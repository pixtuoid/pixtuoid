//! Hook-`command` quoting: `/bin/sh -c` on Unix, `cmd.exe /C` on Windows, and
//! the POSIX splitting Jcode's runner does on both.

use anyhow::Result;

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
/// and the READERS (`verify::shell_shim_ref`, Jcode's `managed_path`). Both
/// surrounding spaces are part of the token, so every site splices/splits on the
/// exact same bytes and a rename can't produce a compiler-invisible write↔read
/// desync (a path with the un-stripped tail baked in reads as "shim missing").
/// The pixtuoid-hook crate parses the BARE argv tokens, a copy
/// `the_shim_parses_every_flag_its_writers_spell` pins.
pub(crate) const SOURCE_FLAG: &str = " --source ";
pub(crate) const EVENT_FLAG: &str = " --event ";
pub(crate) const HOOK_EVENT_FLAG: &str = " --hook-event ";

/// POSIX single-quote a string so a shell treats it as one literal token: an
/// unquoted path containing spaces would split into multiple args and the hook
/// would never be found. On Windows, shell-running targets use
/// `windows_bare_hook_command` instead; Jcode's POSIX-like parser wants it there too.
pub(crate) fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

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
            shell_single_quote(path)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_plain_path() {
        assert_eq!(
            shell_single_quote("/opt/bin/pixtuoid-hook"),
            "'/opt/bin/pixtuoid-hook'"
        );
    }

    #[test]
    fn quotes_path_with_spaces() {
        assert_eq!(
            shell_single_quote("/Users/Jane Doe/bin/pixtuoid-hook"),
            "'/Users/Jane Doe/bin/pixtuoid-hook'"
        );
    }

    #[test]
    fn escapes_embedded_single_quote() {
        // '\'' is close, escaped literal, reopen — the string stays ONE token.
        assert_eq!(shell_single_quote("a'b"), r#"'a'\''b'"#);
    }

    // `verify::posix_unquote` must reverse this byte-for-byte, or the on-disk shim
    // path can't be recovered → a false "shim binary missing" in `doctor` / the
    // Sources panel.
    #[test]
    fn quoting_round_trips_through_the_verify_reader() {
        use crate::install::verify::posix_unquote;
        for p in [
            "/opt/bin/pixtuoid-hook",
            "/Users/Jane Doe/bin/pixtuoid-hook",
            "/opt/it's mine/pixtuoid-hook",
            "/a'b'c/pixtuoid-hook",
            "/weird; path,&(x)/pixtuoid-hook",
        ] {
            assert_eq!(
                posix_unquote(&shell_single_quote(p)),
                p,
                "shell_single_quote → posix_unquote drifted for {p:?}"
            );
        }
    }

    #[cfg(unix)]
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
