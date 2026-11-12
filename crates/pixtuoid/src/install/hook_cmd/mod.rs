//! Hook-`command` quoting: `/bin/sh -c` on Unix, `cmd.exe /C` on Windows, and
//! the POSIX splitting Jcode's runner does on both.

use anyhow::Result;

// Compiled on all platforms so the cmd-safety decision core (with the 8.3
// resolver injected) unit-tests on macOS; only the Win32 FFI is `#[cfg(windows)]`.
mod windows;

/// The DOS 8.3 short form of an existing path, `None` when the call fails. It
/// can keep a metacharacter: a component with no short name comes back long,
/// and one that already fits 8.3 is its own short name (`GetShortPathNameW`
/// Remarks, learn.microsoft.com; [MS-FSCC] "8.3 Filename" bars only space, a
/// second `.`, non-ASCII and length). So every consumer re-checks it.
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
/// `the_shim_reads_every_token_its_writers_spell` pins.
pub(crate) const SOURCE_FLAG: &str = " --source ";
pub(crate) const EVENT_FLAG: &str = " --event ";
pub(crate) const HOOK_EVENT_FLAG: &str = " --hook-event ";

/// The env var the shim reads the source from when no `--source` rides;
/// `the_shim_reads_every_token_its_writers_spell` pins the shim's copy.
pub(crate) const SOURCE_ENV: &str = "PIXTUOID_SOURCE";

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
/// - **Unix**: env-prefix form [`SOURCE_ENV`]`=<source> '<path>'`.
/// - **Windows**: BARE exec form `<path> --source <source>` — cmd.exe can't
///   express the env-prefix, so the source rides the shim's `--source` flag and a
///   space/metacharacter path tries its DOS 8.3 short form (`windows_short_path`).
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
            "{SOURCE_ENV}={source} {}",
            shell_single_quote(path)
        ))
    }
}

/// Whether `command` runs pixtuoid's shim, hand-written or not; one that only names
/// the shim, or hands it to another program, does not. Each form a release wrote is
/// read in its own shape, since one split misreads another's.
pub(crate) fn invokes_shim(command: &str) -> bool {
    exec_form_runs_shim(command) || bare_form_runs_shim(command) || shell_form_runs_shim(command)
}

/// Claude Code's Windows exec form (`claude::hook_command`): the whole `command` is the
/// shim's absolute path, unquoted and spaces included. A character no file name holds
/// (learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file, "Naming Conventions")
/// past the drive means more than one path, as when another program is handed the
/// shim, and such a command is not this form.
fn exec_form_runs_shim(command: &str) -> bool {
    let mut chars = command.chars();
    let drive = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.next() == Some(':')
        && matches!(chars.next(), Some('\\' | '/'));
    drive && !chars.as_str().contains(['<', '>', ':', '"', '|', '?', '*']) && is_shim(command)
}

/// The Windows bare form `<path> --source <name>` ([`shell_hook_command`]): its path is
/// screened free of spaces and never quoted, so its first word is the program, an
/// apostrophe in it included. In place of a cmd-unsafe path it holds the path's 8.3
/// alias, which `--source` and a registered name, the shim's own argument contract,
/// tell from any other program's.
fn bare_form_runs_shim(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return false;
    };
    is_shim(program)
        || (is_short_alias(last_segment(program))
            && words
                .skip_while(|w| *w != SOURCE_FLAG.trim())
                .nth(1)
                .is_some_and(|name| {
                    pixtuoid_core::source::registry::registered_source_names().any(|n| n == name)
                }))
}

/// The Unix forms ([`shell_hook_command`], Claude Code's bare name, Hermes's
/// `'<path>' --source hermes`) split as `/bin/sh` splits them (POSIX Shell Command
/// Language 2.2, which `shell_words::split` implements; a command it can't split is no
/// such form): past any leading `NAME=value` assignments the program is the shim, or
/// one of them is the [`SOURCE_ENV`] stamp only the shim reads, which also finds a shim
/// renamed through [`HOOK_OVERRIDE_ENV`](crate::install::io::HOOK_OVERRIDE_ENV) in a
/// form that carries the stamp.
fn shell_form_runs_shim(command: &str) -> bool {
    let Ok(words) = shell_words::split(command) else {
        return false;
    };
    for word in words {
        match word.split_once('=') {
            Some((name, _)) if is_env_name(name) => {
                if name == SOURCE_ENV {
                    return true;
                }
            }
            _ => return is_shim(&word),
        }
    }
    false
}

/// Whether `path`'s last segment is the shim's binary.
fn is_shim(path: &str) -> bool {
    let bin = crate::install::io::HOOK_BIN;
    let program = last_segment(path);
    program == bin
        || program
            .strip_prefix(bin)
            .is_some_and(|ext| ext.eq_ignore_ascii_case(".exe"))
}

fn last_segment(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// A DOS 8.3 alias of an `.EXE`, `<stem>~<n>.EXE` in any case. The `~<n>` tail is my
/// opinion of the alias Windows generates: [MS-FSCC] 2.1.5.2.1 ("8.3 Filename",
/// learn.microsoft.com) bounds only the name, and [MS-FSA] leaves the alias's string
/// to the implementation.
fn is_short_alias(program: &str) -> bool {
    program
        .to_ascii_uppercase()
        .strip_suffix(".EXE")
        .and_then(|base| base.rsplit_once('~'))
        .is_some_and(|(stem, n)| !stem.is_empty() && n.parse::<u32>().is_ok())
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_a_release_wrote_invokes_the_shim_and_a_lookalike_does_not() {
        for ours in [
            "PIXTUOID_SOURCE=claude-code 'pixtuoid-hook'",
            "pixtuoid-hook",
            "PIXTUOID_SOURCE=reasonix '/Users/Jane Doe/bin/pixtuoid-hook'",
            "'/opt/bin/pixtuoid-hook' --source hermes",
            "PIXTUOID_SOURCE=antigravity '/opt/pixtuoid-hook' --event Stop",
            r"C:\tools\pixtuoid-hook.exe --source claude-code",
            r#""C:\Program Files\pixtuoid\pixtuoid-hook.exe""#,
            r"c:\progra~1\pixtuo~1\pixtuo~12.exe --source codex",
            "PIXTUOID_SOURCE=codex '/opt/shims/pxhook'",
            r"C:\Users\Jane Doe\.cargo\bin\pixtuoid-hook.exe",
            r"C:\Users\O'Brien\.cargo\bin\pixtuoid-hook.exe --source codex",
        ] {
            assert!(invokes_shim(ours), "{ours}");
        }
        for theirs in [
            "notify-send done",
            "/opt/pixtuoid-hook-wrapper",
            "my-pixtuoid-hook",
            "echo pixtuoid-hook",
            "printf 'PIXTUOID_SOURCE=x'",
            "my-tool --hook /opt/bin/pixtuoid-hook",
            "'my tool' 'pixtuoid-hook' --source x",
            "FOO=1 echo PIXTUOID_SOURCE=x",
            r"C:\PROGRA~1\PIXTUO~1\PIXTUO~2.EXE",
            r"C:\PIXTUO~.EXE --source codex",
            r"C:\PROGRA~1\PIXTUO~1\PIXTUO~2.EXE --source notasource",
            r"C:\tools\wrap.exe C:\bin\pixtuoid-hook.exe",
        ] {
            assert!(!invokes_shim(theirs), "{theirs}");
        }
    }

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
            r"/opt/a\b/pixtuoid-hook",
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
    use super::{EVENT_FLAG, HOOK_EVENT_FLAG, SOURCE_ENV, SOURCE_FLAG};

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
    fn the_shim_reads_every_token_its_writers_spell() {
        let shim = include_str!("../../../../pixtuoid-hook/src/main.rs");
        for token in [SOURCE_FLAG, EVENT_FLAG, HOOK_EVENT_FLAG, SOURCE_ENV] {
            let bare = format!("\"{}\"", token.trim());
            assert!(shim.contains(&bare), "the shim reads no {bare}");
        }
    }
}
