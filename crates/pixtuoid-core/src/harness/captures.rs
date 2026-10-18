//! The committed capture corpus: where each capture's bytes live and which
//! registered source owns them, read off the tree's layout; and the recorder's
//! contract with its callers: its workspace, raw record and flag spellings.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::source::registry;

/// Every `.jsonl` under `root` that this source's registry `path_filter` admits,
/// recursed without following a symlinked entry.
///
/// The per-entry rule is `admit::classify`, shared with the production watcher —
/// the recorder COMMITS one of these files as a golden, so a predicate it did
/// not apply would let a fixture teach the decoder a shape production never
/// reads. Only the traversal is local.
pub fn transcripts_under(source: &str, root: &Path) -> Vec<PathBuf> {
    fn walk(admits: &dyn Fn(&Path) -> bool, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            match crate::source::admit::classify(&meta, &p, admits) {
                crate::source::admit::Entry::Recurse => walk(admits, &p, out),
                crate::source::admit::Entry::Take => out.push(p),
                _ => {}
            }
        }
    }
    let admits = registry::path_filter_for(source);
    let mut out = Vec::new();
    walk(&admits, root, &mut out);
    out
}

/// Where every capture's CLI runs. The path is FIXED and generic on purpose:
/// every payload embeds its own cwd, the decoders read it, so the strip keeps it —
/// capturing somewhere already generic is what keeps a per-run path out of the
/// bytes. The recorder prepares it; a hermetic script names files under it.
///
/// On Windows it sits under ProgramData, outside every user's profile: CC reads
/// config from each ancestor of its cwd, a home's `.claude` included.
///
/// # Errors
///
/// On Windows, if the shell has no ProgramData, as in an environment cleared
/// of the host's.
pub fn capture_workspace() -> std::io::Result<PathBuf> {
    #[cfg(windows)]
    let root = crate::harness::host::known_folder(crate::harness::host::KnownFolder::ProgramData)?;
    #[cfg(not(windows))]
    let root = PathBuf::from("/tmp");
    Ok(root.join(capture_workspace_tail()))
}

/// The end every [`capture_workspace`] path has, whatever the host's root: the
/// part [`capture_base`] checks.
pub fn capture_workspace_tail() -> PathBuf {
    Path::new(CAPTURE_BASE).join(CAPTURE_PROJ)
}

/// The directory a capture workspace sits in, which a recorder clears and
/// recreates.
const CAPTURE_BASE: &str = "pixtuoid-capture";
/// The workspace's own name inside [`CAPTURE_BASE`].
const CAPTURE_PROJ: &str = "proj";

/// The base a recorder may clear for the workspace `ws`: only that of an
/// absolute path ending in [`capture_workspace_tail`], so a path an operator
/// names can clear only a directory named as [`capture_workspace`]'s base is.
pub fn capture_base(ws: &Path) -> Option<&Path> {
    (ws.is_absolute() && ws.ends_with(capture_workspace_tail()))
        .then(|| ws.parent())
        .flatten()
}

/// The command `descriptor`'s CLI answers its version to: a capture anchors
/// that version, so a row without one can't be recorded.
///
/// # Errors
///
/// If the row has no version probe.
pub fn required_version_probe(
    descriptor: &crate::source::registry::SourceDescriptor,
) -> std::io::Result<&'static [&'static str]> {
    descriptor
        .version_probe
        .ok_or_else(|| std::io::Error::other(format!("{} has no version probe", descriptor.name)))
}

/// Why this host may not write the committed corpus, which pins Unix
/// realpaths and cwds.
pub const CORPUS_REFUSAL: Option<&str> = if cfg!(unix) {
    None
} else {
    Some("the committed corpus pins Unix paths: record on macOS or Linux")
};

/// What a `--version` run answered: the first non-blank line of a successful
/// run's stdout.
pub fn version_answer(out: &std::process::Output) -> Option<String> {
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())?
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

/// The `NOTE.txt` the recorder writes into the workspace: what every scripted
/// turn reads, and what a script waits for before it replies.
pub const CAPTURE_NOTE: &str = "pong";

/// A capture's hook payloads, one per line as the shim sent them.
pub const HOOK_PAYLOADS: &str = "hook-payloads.jsonl";
/// A committed capture's declaration of what ran and how it was de-identified.
pub const PROVENANCE: &str = "provenance.json";
/// A raw capture's [`CaptureRecord`].
pub const CAPTURE_RECORD: &str = "capture.json";

/// The recorder's (`capture_fixture`) options, as its callers spell them; each
/// does what the recorder's `Options` field of its name says.
pub mod recorder_flag {
    /// `Options::out`.
    pub const OUT: &str = "--out";
    /// `Options::tee`.
    pub const TEE: &str = "--tee";
    /// `Options::within`.
    pub const WITHIN: &str = "--within";
    /// `Options::env`.
    pub const ENV: &str = "--env";
    /// `Options::promote`.
    pub const PROMOTE: &str = "--promote";
    /// `Options::cli_version`.
    pub const CLI_VERSION: &str = "--cli-version";
    /// `Options::workspace`.
    pub const WORKSPACE: &str = "--workspace";
    /// `Options::prepare_root`.
    pub const PREPARE_ROOT: &str = "--prepare-root";
}

/// What only the recorder knows about a raw (`--out`) capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRecord {
    /// The workspace the CLI ran in, canonicalized.
    pub workspace: PathBuf,
    /// The CLI the recorder drove.
    pub cli: String,
    /// Its `--version`.
    pub version: String,
    /// Its exit code; `None` when a signal ended it.
    pub exit_code: Option<i32>,
}

/// Module dirs whose name is NOT the registered source id.
const MODULE_TO_SOURCE: &[(&str, &str)] = &[("claude", "claude-code")];

/// A committed capture: the directory holding its bytes and the registered
/// source the LAYOUT says owns them — never the provenance, which could lie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// The directory holding the capture's bytes and its `provenance.json`.
    pub dir: PathBuf,
    /// The registered source id that owns the bytes.
    pub source: String,
}

impl Capture {
    /// Every `.jsonl` under the capture, sorted. Recursive: copilot and grok keep
    /// a scenario's transcript under its session directory, beside the hook
    /// payloads at the top.
    pub(crate) fn jsonl_files(&self) -> Vec<PathBuf> {
        jsonl_files_under(&self.dir)
    }
}

/// Every `.jsonl` under `dir`, recursively, sorted.
pub fn jsonl_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        for p in std::fs::read_dir(d)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
        {
            if p.is_dir() {
                pending.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                files.push(p);
            }
        }
    }
    files.sort();
    files
}

/// THE enumeration of committed captures under a `tests/sources` tree, from the
/// layout alone: the provenance gates walk it and the recorder re-strips it, so
/// a fix lands on the whole population. Three shapes: `fixtures/<source>/<scenario>/`;
/// `<module>/fixtures/` declaring at its root; `<module>/fixtures/<sub>/`, owned by
/// the module when it is itself a registered source (omp's scenario tree), else
/// by the sub (delegation's per-source rule families). `decode/` holds hand-built
/// decoder inputs, not captures. A module `fixtures/` that declares nothing at
/// its root and holds no sub-tree is returned as a capture, so the provenance
/// gate names it rather than the walk losing it.
pub fn captures(root: &Path) -> Vec<Capture> {
    fn dirs(dir: &Path) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        out.sort();
        out
    }
    fn name(p: &Path) -> String {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
    fn source_id(raw: &str) -> String {
        MODULE_TO_SOURCE
            .iter()
            .find(|(from, _)| *from == raw)
            .map_or(raw, |(_, to)| *to)
            .to_string()
    }
    let mut out = Vec::new();
    for source_dir in dirs(&root.join("fixtures")) {
        let source = source_id(&name(&source_dir));
        out.extend(dirs(&source_dir).into_iter().map(|dir| Capture {
            dir,
            source: source.clone(),
        }));
    }
    for module in dirs(root) {
        let module_name = name(&module);
        if module_name == "decode" || module_name == "fixtures" {
            continue;
        }
        let fixtures = module.join("fixtures");
        if !fixtures.is_dir() {
            continue;
        }
        let subs = dirs(&fixtures);
        if fixtures.join(PROVENANCE).is_file() || subs.is_empty() {
            out.push(Capture {
                dir: fixtures,
                source: source_id(&module_name),
            });
            continue;
        }
        let module_is_source = registry::descriptor_for(&module_name).is_some();
        out.extend(subs.into_iter().map(|sub| {
            let raw = if module_is_source {
                module_name.clone()
            } else {
                name(&sub)
            };
            Capture {
                source: source_id(&raw),
                dir: sub,
            }
        }));
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_capture_workspace_names_a_base_to_clear() {
        let ws = capture_workspace().expect("the workspace");
        assert_eq!(capture_base(&ws), ws.parent());
        let root = std::env::temp_dir();
        let named = root.join(CAPTURE_BASE).join(CAPTURE_PROJ);
        assert_eq!(
            capture_base(&named),
            Some(root.join(CAPTURE_BASE).as_path())
        );
        assert_eq!(
            capture_base(&root.join("proj")),
            None,
            "its parent is the operator's"
        );
        assert_eq!(
            capture_base(&Path::new(CAPTURE_BASE).join(CAPTURE_PROJ)),
            None,
            "relative"
        );
    }

    #[test]
    fn a_version_is_the_first_line_a_successful_run_printed() {
        let ran = |ok: bool, stdout: &str| std::process::Output {
            status: if ok {
                std::process::ExitStatus::default()
            } else {
                failed()
            },
            stdout: stdout.into(),
            stderr: Vec::new(),
        };
        assert_eq!(
            version_answer(&ran(true, "\n 2.0.1 \nbuilt today\n")).as_deref(),
            Some("2.0.1")
        );
        assert_eq!(
            version_answer(&ran(true, " \n")),
            None,
            "it printed nothing"
        );
        assert_eq!(version_answer(&ran(false, "2.0.1\n")), None, "it failed");
    }

    fn failed() -> std::process::ExitStatus {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;
        // A Unix raw status is a wait(2) word, exit code in its high byte; a
        // Windows one is the exit code itself.
        std::process::ExitStatus::from_raw(if cfg!(unix) { 1 << 8 } else { 1 })
    }

    #[test]
    fn the_walk_scales_past_the_size_that_broke_the_shell_version() {
        // No pipe here — this pins the SHAPE, not a buffer size.
        let d = tempfile::tempdir().expect("tempdir");
        for i in 0..1200 {
            std::fs::write(d.path().join(format!("f{i}.jsonl")), "{}").expect("write");
        }
        assert_eq!(transcripts_under("claude-code", d.path()).len(), 1200);
    }

    // Unix-only: creating a directory symlink on Windows needs a separate API and
    // a privilege this suite does not assume, and asserting without one made the
    // test claim a platform it never exercised.
    #[cfg(unix)]
    #[test]
    fn the_walk_does_not_follow_a_directory_symlink() {
        let d = tempfile::tempdir().expect("tempdir");
        let real = d.path().join("real");
        std::fs::create_dir(&real).expect("mkdir");
        std::fs::write(real.join("a.jsonl"), "{}").expect("write");
        std::os::unix::fs::symlink(&real, d.path().join("link")).expect("symlink");
        assert_eq!(
            transcripts_under("claude-code", d.path()).len(),
            1,
            "the symlinked copy must not be walked twice"
        );
    }

    #[test]
    fn the_walk_applies_the_sources_own_path_filter() {
        // grok writes five jsonl siblings per session; one is the transcript.
        let d = tempfile::tempdir().expect("tempdir");
        for name in ["updates.jsonl", "rewind_points.jsonl", "notes.txt"] {
            std::fs::write(d.path().join(name), "{}").expect("write");
        }
        let got = transcripts_under("grok", d.path());
        assert_eq!(got.len(), 1, "only the transcript is admitted, got {got:?}");
        assert!(got[0].ends_with("updates.jsonl"));
    }
}
