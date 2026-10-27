//! Install-schema verification — the "silent-dead source" detector: a source the
//! user CONNECTED renders ZERO sprites when its install is structurally broken.
//!
//! Per-source FORMAT knowledge stays in each `install/<target>.rs` `verify_schema`
//! (invariant #3) — this module holds only the shared READ-side machinery.

use std::path::PathBuf;

/// How a managed hook command references the shim binary — extracted PURELY from
/// the config content; the filesystem check is layered on by
/// `install::verify_target`, the only part with I/O.
#[derive(Debug, Default, PartialEq, Eq)]
pub enum ShimRef {
    Absolute(PathBuf),
    /// A bare name relying on PATH resolution → soft check only: a doctor-process
    /// PATH miss is NOT proof the CLI can't resolve it.
    BareName,
    #[default]
    Unknown,
}

/// Pull the baked shim path back out of a RENDERED code artifact's
/// `const HOOK_PATH … = "<json string>"` line.
pub(crate) fn baked_hook_path(content: &str) -> Option<PathBuf> {
    baked_hook_path_after(content, BAKED_HOOK_MARKER)
}

/// [`baked_hook_path`] for an artifact whose declaration starts with `decl`.
pub(crate) fn baked_hook_path_after(content: &str, decl: &str) -> Option<PathBuf> {
    // Anchor on the DECLARATION, not a mention: a line that merely contains
    // it would otherwise match first, yield no JSON literal, and downgrade the
    // moved-shim HARD check to a soft "could not read" note.
    let line = content.lines().find(|l| l.trim_start().starts_with(decl))?;
    let literal = line.split_once('=')?.1.trim().trim_end_matches(';').trim();
    let path: String = serde_json::from_str(literal).ok()?;
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub(crate) fn baked_shim(content: &str) -> ShimRef {
    baked_hook_path(content).map_or(ShimRef::Unknown, ShimRef::Absolute)
}

/// All there is to check of a code artifact: its sentinel, a substituted
/// placeholder, and the `shim` read back from its baked `HOOK_PATH`.
pub(crate) fn sentinel_plugin_schema(
    plugin: &crate::install::merge::SentinelPlugin,
    content: &str,
    shim: ShimRef,
) -> SchemaParse {
    let what = plugin.what;
    if !content.contains(plugin.sentinel) {
        return SchemaParse::broken(format!(
            "{what} is missing or replaced (sentinel absent) — reconnect {}",
            plugin.cli
        ));
    }
    if content.contains(crate::install::merge::HOOK_PLACEHOLDER) {
        return SchemaParse::broken(format!(
            "{what}'s shim-path placeholder was never substituted"
        ));
    }
    match shim {
        ShimRef::Unknown => SchemaParse::broken(format!("could not read HOOK_PATH from {what}")),
        shim => SchemaParse {
            shim,
            ..Default::default()
        },
    }
}

/// Marks a rendered artifact that BAKES the shim path. Matched against the
/// INTENDED content the installer renders, never the file on disk.
pub(crate) const BAKED_HOOK_MARKER: &str = "const HOOK_PATH";

/// The PURE, config-content-only half of a verification. `issues` are HARD
/// problems (definitely broken); `notes` are SOFT ones — real and actionable, but
/// not proof of a break.
#[derive(Debug, Default)]
pub struct SchemaParse {
    pub issues: Vec<String>,
    pub notes: Vec<String>,
    pub shim: ShimRef,
}

impl SchemaParse {
    /// A target whose config failed to parse / had no managed entries at all.
    pub fn broken(issue: impl Into<String>) -> Self {
        SchemaParse {
            issues: vec![issue.into()],
            ..Default::default()
        }
    }
}

/// The full install-schema verdict for one target: config-level HARD `issues`
/// plus the shim-on-disk check, and any SOFT `notes` — environment-dependent,
/// never a "broken" verdict on their own.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SchemaVerifyResult {
    pub issues: Vec<String>,
    /// Shown by `doctor`, never the boot warning.
    pub notes: Vec<String>,
}

impl SchemaVerifyResult {
    /// Sound iff there are no HARD issues (soft notes don't count).
    pub(crate) fn is_sound(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Assemble a `SchemaParse` from a per-target scan, centralizing the issue
/// wording so every target reports consistently.
pub(crate) fn assemble(
    missing_events: &[&str],
    any_managed: bool,
    shim: ShimRef,
    extra: Vec<String>,
) -> SchemaParse {
    let mut issues = extra;
    if !any_managed {
        issues.push(
            "no managed pixtuoid hook entries (the `_pixtuoid` sentinel is absent — the config \
             was hand-edited or hooks were never installed)"
                .into(),
        );
    } else if !missing_events.is_empty() {
        issues.push(format!(
            "missing hook entries for: {} (an older pixtuoid install, or an upstream config-schema \
             change, orphaned them — reconnect via the Sources panel)",
            missing_events.join(", ")
        ));
    }
    SchemaParse {
        issues,
        shim,
        ..Default::default()
    }
}

/// Verify Cursor's flat-JSON hook config: `hooks.<event>` is an array of
/// `{_pixtuoid: true, command, …}` entries.
pub(crate) fn flat_json_verify(content: &str, events: &[&str], sentinel: &str) -> SchemaParse {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(content) else {
        return SchemaParse::broken("hooks config no longer parses as JSON");
    };
    let hooks = doc.get("hooks").and_then(|h| h.as_object());
    let mut missing = Vec::new();
    let mut any = false;
    let mut shim = ShimRef::Unknown;
    for ev in events {
        let managed = hooks
            .and_then(|h| h.get(*ev))
            .and_then(|a| a.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|e| e.get(sentinel).and_then(|v| v.as_bool()) == Some(true))
            });
        match managed {
            Some(entry) => {
                any = true;
                if shim == ShimRef::Unknown {
                    shim = entry
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(shell_shim_ref)
                        .unwrap_or(ShimRef::Unknown);
                }
            }
            None => missing.push(*ev),
        }
    }
    assemble(&missing, any, shim, vec![])
}

/// Extract the shim path from a shell-form managed command — the form
/// `hook_cmd::shell_hook_command` writes: Unix `PIXTUOID_SOURCE=<source> '<abs>'`
/// (single-quoted) and Windows `<abs> --source <source>` (bare), either with an
/// optional trailing ` --event <name>` (CodeWhale) or ` --hook-event <name>` (agy).
pub(crate) fn shell_shim_ref(command: &str) -> ShimRef {
    use crate::install::hook_cmd::{EVENT_FLAG, HOOK_EVENT_FLAG, SOURCE_FLAG};
    // Strip the trailing event flag with rsplit_once, and only when the
    // residual head still parses as a writer shape (ends with `'` = Unix quoted
    // form, contains " --source " = Windows bare form): a quoted path that
    // literally contains the flag would otherwise be cut mid-path into a bogus
    // partial token that never `.exists()`.
    let head = [EVENT_FLAG, HOOK_EVENT_FLAG]
        .into_iter()
        .find_map(|flag| match command.rsplit_once(flag) {
            Some((before, _)) if before.ends_with('\'') || before.contains(SOURCE_FLAG) => {
                Some(before)
            }
            _ => None,
        })
        .unwrap_or(command);
    // The trailing-quote test discriminates the two forms unambiguously: the Unix
    // form is single-quoted by `shell_single_quote` so it ENDS with `'`, while a
    // Windows bare path never does even when it CONTAINS apostrophes
    // (`C:\O'Brien\…`). Decode by REVERSING the escaping (`'\''` → `'`) rather
    // than splitting on whitespace, which would truncate a spaced path.
    if head.ends_with('\'')
        && let Some(start) = head.find('\'')
    {
        let end = head.len() - 1; // the closing `'` (a 1-byte apostrophe)
        if start < end {
            let p = posix_unquote(&head[start..=end]);
            return if p.is_empty() {
                ShimRef::Unknown
            } else {
                ShimRef::Absolute(PathBuf::from(p))
            };
        }
    }
    if let Some((path, _)) = head.split_once(crate::install::hook_cmd::SOURCE_FLAG) {
        let p = path.trim();
        return if p.is_empty() {
            ShimRef::Unknown
        } else {
            ShimRef::Absolute(PathBuf::from(p))
        };
    }
    // Unquoted fallback (hand-edited configs): the last whitespace token —
    // `split_whitespace` never yields an empty one, so no emptiness guard.
    match head.split_whitespace().next_back() {
        Some(tok) => ShimRef::Absolute(PathBuf::from(tok)),
        None => ShimRef::Unknown,
    }
}

/// Reverse `hook_cmd::unix::shell_single_quote` over a span that STARTS and ENDS
/// on a `'`, so a literal `'\''` decodes to a single `'` and an in-quote space
/// stays literal.
pub(crate) fn posix_unquote(span: &str) -> String {
    let mut out = String::new();
    let mut in_quote = false;
    let mut chars = span.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => in_quote = !in_quote,
            '\\' if !in_quote => {
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn baked_hook_path_anchors_on_the_declaration_not_a_mention() {
        assert_eq!(
            baked_hook_path(
                "// HOOK_PATH is baked in — see `const HOOK_PATH` below\n\
                 const HOOK_PATH = \"/opt/bin/pixtuoid-hook\";\n"
            ),
            Some(PathBuf::from("/opt/bin/pixtuoid-hook"))
        );
        assert_eq!(
            baked_hook_path("const HOOK_PATH: string = \"/opt/hook\"\n"),
            Some(PathBuf::from("/opt/hook"))
        );
        assert_eq!(baked_hook_path("const HOOK_PATH = \"\";\n"), None);
        assert_eq!(baked_hook_path("// no binding at all\n"), None);
        assert_eq!(
            baked_hook_path_after(
                "# HOOK_PATH is baked in at install time\nHOOK_PATH = \"/opt/hook\"\n",
                "HOOK_PATH ="
            ),
            Some(PathBuf::from("/opt/hook"))
        );
    }

    #[test]
    fn shell_shim_ref_unix_env_prefix_form() {
        assert_eq!(
            shell_shim_ref("PIXTUOID_SOURCE=codex '/opt/pixtuoid-hook'"),
            ShimRef::Absolute(PathBuf::from("/opt/pixtuoid-hook"))
        );
    }

    #[test]
    fn shell_shim_ref_unix_quoted_path_containing_source_marker_is_not_missplit() {
        assert_eq!(
            shell_shim_ref("PIXTUOID_SOURCE=codex '/opt/my --source dir/pixtuoid-hook'"),
            ShimRef::Absolute(PathBuf::from("/opt/my --source dir/pixtuoid-hook"))
        );
    }

    #[test]
    fn shell_shim_ref_windows_bare_path_with_apostrophes_is_not_mis_quoted() {
        assert_eq!(
            shell_shim_ref(r"C:\Bob's O'Brien\pixtuoid-hook.exe --source reasonix"),
            ShimRef::Absolute(PathBuf::from(r"C:\Bob's O'Brien\pixtuoid-hook.exe"))
        );
        assert_eq!(
            shell_shim_ref(r"C:\O'Brien\hook.exe --source codewhale --event session_start"),
            ShimRef::Absolute(PathBuf::from(r"C:\O'Brien\hook.exe"))
        );
    }

    #[test]
    fn shell_shim_ref_lone_trailing_quote_is_not_a_quoted_span() {
        assert_eq!(
            shell_shim_ref("/opt/hook'"),
            ShimRef::Absolute(PathBuf::from("/opt/hook'"))
        );
    }

    #[test]
    fn shell_shim_ref_unix_recovers_spaced_single_quoted_path() {
        assert_eq!(
            shell_shim_ref("PIXTUOID_SOURCE=codex '/Users/Jane Doe/bin/pixtuoid-hook'"),
            ShimRef::Absolute(PathBuf::from("/Users/Jane Doe/bin/pixtuoid-hook"))
        );
    }

    #[test]
    fn shell_shim_ref_unix_recovers_spaced_path_with_event_tail() {
        assert_eq!(
            shell_shim_ref(
                "PIXTUOID_SOURCE=codewhale '/Users/Jane Doe/hook' --event session_start"
            ),
            ShimRef::Absolute(PathBuf::from("/Users/Jane Doe/hook"))
        );
    }

    #[test]
    fn shell_shim_ref_unix_recovers_path_with_embedded_single_quote() {
        assert_eq!(
            shell_shim_ref(r#"PIXTUOID_SOURCE=codex '/U/O'\''B/hook'"#),
            ShimRef::Absolute(PathBuf::from("/U/O'B/hook"))
        );
    }

    #[test]
    fn shell_shim_ref_windows_bare_form() {
        assert_eq!(
            shell_shim_ref(r"C:\bin\pixtuoid-hook.exe --source reasonix"),
            ShimRef::Absolute(PathBuf::from(r"C:\bin\pixtuoid-hook.exe"))
        );
    }

    #[test]
    fn shell_shim_ref_strips_codewhale_event_tail() {
        assert_eq!(
            shell_shim_ref("PIXTUOID_SOURCE=codewhale '/opt/pixtuoid-hook' --event session_start"),
            ShimRef::Absolute(PathBuf::from("/opt/pixtuoid-hook"))
        );
        assert_eq!(
            shell_shim_ref(r"C:\bin\pixtuoid-hook.exe --source codewhale --event session_start"),
            ShimRef::Absolute(PathBuf::from(r"C:\bin\pixtuoid-hook.exe"))
        );
    }

    #[test]
    fn shell_shim_ref_path_containing_event_marker_is_not_missplit() {
        assert_eq!(
            shell_shim_ref(
                "PIXTUOID_SOURCE=codewhale '/Users/x/my --event dir/pixtuoid-hook' --event tool"
            ),
            ShimRef::Absolute(PathBuf::from("/Users/x/my --event dir/pixtuoid-hook"))
        );
    }

    #[test]
    fn shell_shim_ref_tailless_path_containing_event_marker_is_not_missplit() {
        assert_eq!(
            shell_shim_ref("PIXTUOID_SOURCE=codex '/opt/my --event dir/pixtuoid-hook'"),
            ShimRef::Absolute(PathBuf::from("/opt/my --event dir/pixtuoid-hook"))
        );
        assert_eq!(
            shell_shim_ref(r"C:\my --event dir\pixtuoid-hook.exe --source cursor"),
            ShimRef::Absolute(PathBuf::from(r"C:\my --event dir\pixtuoid-hook.exe"))
        );
    }

    #[test]
    fn shell_shim_ref_empty_is_unknown() {
        assert_eq!(shell_shim_ref(""), ShimRef::Unknown);
    }

    #[test]
    fn shell_shim_ref_windows_empty_path_is_unknown() {
        assert_eq!(shell_shim_ref(" --source reasonix"), ShimRef::Unknown);
    }

    #[test]
    fn shell_shim_ref_unix_empty_quoted_token_is_unknown() {
        assert_eq!(shell_shim_ref("PIXTUOID_SOURCE=codex ''"), ShimRef::Unknown);
    }

    #[test]
    fn schema_parse_broken_sets_issue_and_unknown_shim() {
        let p = SchemaParse::broken("boom");
        assert_eq!(p.issues, vec!["boom".to_string()]);
        assert_eq!(p.shim, ShimRef::Unknown);
    }

    #[test]
    fn flat_json_verify_reports_broken_on_invalid_json() {
        let p = flat_json_verify("{not json", &["PreToolUse"], "_pixtuoid");
        assert_eq!(p.shim, ShimRef::Unknown);
        assert!(
            p.issues
                .iter()
                .any(|i| i.contains("no longer parses as JSON")),
            "{:?}",
            p.issues
        );
    }

    #[test]
    fn flat_json_verify_flags_event_missing_a_managed_entry() {
        let content = json!({
            "hooks": {
                "PreToolUse": [{
                    "_pixtuoid": true,
                    "command": "PIXTUOID_SOURCE=reasonix '/opt/pixtuoid-hook'"
                }]
            }
        })
        .to_string();
        let p = flat_json_verify(&content, &["PreToolUse", "PostToolUse"], "_pixtuoid");
        assert!(
            p.issues
                .iter()
                .any(|i| i.contains("missing hook entries for") && i.contains("PostToolUse")),
            "{:?}",
            p.issues
        );
        assert_eq!(
            p.shim,
            ShimRef::Absolute(PathBuf::from("/opt/pixtuoid-hook"))
        );
    }

    #[test]
    fn schema_verify_soundness_ignores_notes() {
        let clean = SchemaVerifyResult::default();
        assert!(clean.is_sound());
        let soft = SchemaVerifyResult {
            issues: vec![],
            notes: vec!["pixtuoid-hook not on PATH".into()],
        };
        assert!(
            soft.is_sound(),
            "soft notes must not make an install 'broken'"
        );
        let hard = SchemaVerifyResult {
            issues: vec!["shim missing".into()],
            notes: vec![],
        };
        assert!(!hard.is_sound());
    }
}
