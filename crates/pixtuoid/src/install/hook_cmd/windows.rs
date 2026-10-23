//! Windows half of the shell-hook-command quoting primitives.
//!
//! The cmd-safety decision core is compiled on every OS — with the 8.3 resolver
//! injected — so every branch unit-tests without the Win32 FFI. Only the FFI
//! itself and the entry point that wires it in are `#[cfg(windows)]`.

use anyhow::Result;

/// Characters special to cmd.exe's parser, hence unsafe in an UNQUOTED hook
/// path: the first-token delimiters (space, tab, `;`, `,`, `=` — the last three
/// are legal in an NTFS filename) TRUNCATE the command; the rest inject or
/// redirect. `!` is deliberately excluded — special only under delayed expansion
/// (`cmd /V:ON`), which these hook runners don't enable.
const CMD_UNSAFE: &[char] = &[
    ' ', '\t', ';', ',', '=', '"', '&', '|', '<', '>', '(', ')', '^', '%',
];

#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "only the Windows installer calls it off test")
)]
fn first_cmd_unsafe_char(p: &str) -> Option<char> {
    p.chars().find(|c| CMD_UNSAFE.contains(c))
}

/// The BARE Windows hook `command` for a CLI whose hook runner shells via
/// `cmd.exe /C`. Form: `<path> --source <name>` — the source
/// rides as a flag because cmd.exe has no `VAR=value cmd` env-prefix. The path is
/// UNQUOTED because a quoted one can't survive `cmd /C` (the host's argv-quoting
/// escapes `"`→`\"`, which cmd then mangles), so cmd PARSES the path and any
/// cmd-special char in it would truncate or inject — hence the 8.3 substitution.
/// `source` is a registered source name: `every_registered_source_name_is_a_plain_identifier`
/// pins it shell/cmd-safe, so it splices unquoted.
#[cfg(windows)]
pub(super) fn windows_bare_hook_command(resolved_path: &str, source: &str) -> Result<String> {
    resolve_windows_command(resolved_path, source, short_path_windows)
}

#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "only the Windows installer calls it off test")
)]
fn resolve_windows_command(
    path: &str,
    source: &str,
    short_path: impl FnOnce(&str) -> Option<String>,
) -> Result<String> {
    let Some(bad) = first_cmd_unsafe_char(path) else {
        return Ok(format!("{path}{}{source}", super::SOURCE_FLAG));
    };
    // With 8.3 generation disabled on the volume GetShortPathNameW returns the
    // long path unchanged, so the short form is re-checked before it is accepted.
    if let Some(s) = short_path(path)
        && first_cmd_unsafe_char(&s).is_none()
    {
        return Ok(format!("{s}{}{source}", super::SOURCE_FLAG));
    }
    anyhow::bail!(
        "pixtuoid-hook is at a path containing {bad:?} ({path}) that the cmd.exe /C hook \
         runner can't safely invoke, and no DOS 8.3 short name is available (8.3 \
         generation is disabled on this volume). Install pixtuoid to a path of ordinary \
         characters (e.g. %USERPROFILE%\\.cargo\\bin or the npm global prefix) and \
         reconnect the target in pixtuoid's Sources panel. (Tracking: #195.)"
    );
}

#[cfg(windows)]
pub(super) fn short_path_windows(long: &str) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

    let wide: Vec<u16> = std::ffi::OsStr::new(long)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a valid NUL-terminated UTF-16 string. Passing a null
    // out-buffer with length 0 is the documented "return required size (incl. NUL)"
    // probe; it writes nothing and returns 0 on failure.
    let needed = unsafe { GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u16; needed as usize];
    // SAFETY: `buf` has `needed` u16 slots; the API writes at most `needed-1` chars
    // plus a NUL and returns the count written (excl. NUL), or 0 on failure.
    let written = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), needed) };
    if written == 0 || written >= needed {
        return None;
    }
    String::from_utf16(&buf[..written as usize]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_command_is_bare_for_a_clean_path() {
        let c = resolve_windows_command(r"C:\tools\pixtuoid-hook.exe", "codex", |_| {
            panic!("short_path must NOT be called for a clean path")
        });
        assert_eq!(c.unwrap(), r"C:\tools\pixtuoid-hook.exe --source codex");
    }

    #[test]
    fn windows_command_uses_8dot3_short_form_when_path_has_a_space() {
        let c =
            resolve_windows_command(r"C:\Program Files\x\pixtuoid-hook.exe", "reasonix", |_| {
                Some(r"C:\PROGRA~1\x\PIXTUO~1.EXE".to_string())
            });
        assert_eq!(c.unwrap(), r"C:\PROGRA~1\x\PIXTUO~1.EXE --source reasonix");
    }

    #[test]
    fn windows_command_rejects_when_8dot3_is_unavailable() {
        // 8.3 disabled → resolver returns the long (still-unsafe) path → reject.
        let long = resolve_windows_command(r"C:\Program Files\x\h.exe", "codex", |p| {
            Some(p.to_string())
        });
        assert!(long.is_err());
        let none = resolve_windows_command(r"C:\a&b\h.exe", "codex", |_| None);
        let err = none.unwrap_err().to_string();
        assert!(
            err.contains("cmd.exe") && err.contains("ordinary characters"),
            "reject message must stay actionable: {err}"
        );
    }

    #[test]
    fn windows_command_treats_cmd_first_token_delimiters_as_unsafe() {
        // `;` `,` `=` are legal Win32 filename chars and tab survives at the
        // NTFS layer, so a space-free path can genuinely carry one.
        for path in [
            r"C:\tools\a;b\pixtuoid-hook.exe",
            r"C:\tools\a,b\pixtuoid-hook.exe",
            r"C:\tools\a=b\pixtuoid-hook.exe",
            "C:\\tools\\a\tb\\pixtuoid-hook.exe",
        ] {
            let short = resolve_windows_command(path, "codex", |_| {
                Some(r"C:\TOOLS\SHORT~1\PIXTUO~1.EXE".to_string())
            });
            assert_eq!(
                short.unwrap(),
                r"C:\TOOLS\SHORT~1\PIXTUO~1.EXE --source codex",
                "{path:?} must substitute the 8.3 short name"
            );
            let rejected = resolve_windows_command(path, "codex", |_| None);
            assert!(
                rejected.is_err(),
                "{path:?} must reject, never write a silently-truncating bare form"
            );
        }
    }

    // With 8.3 disabled the API returns the long path — still `Some` — so this can
    // only pin that the two-call length-then-fill pattern doesn't mis-size.
    #[cfg(windows)]
    #[test]
    fn short_path_windows_resolves_an_existing_dir() {
        let tmp = std::env::temp_dir();
        let got = short_path_windows(&tmp.to_string_lossy());
        assert!(
            got.is_some_and(|s| !s.is_empty()),
            "GetShortPathNameW must resolve an existing dir to a non-empty path"
        );
    }
}
