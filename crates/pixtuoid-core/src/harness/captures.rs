//! The committed capture corpus: where each capture's bytes live and which
//! registered source owns them, read off the tree's layout, and what they say
//! blind to the ids their run minted; and the recorder's contract with its
//! callers: its workspace, raw record, flag spellings and the wait on a
//! server either starts.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// The one transcript of `source`'s under a capture `dir`, found rather than
/// named: a re-record gives it a new filename. The registry's admit rule and
/// the recorder's own hook-envelope test decide what is a transcript.
///
/// # Errors
///
/// Every transcript found, unless that is exactly one: a stale second one
/// beside a promoted `.new` fails here instead of being picked by directory
/// order.
pub fn transcript_in(source: &str, dir: &Path) -> Result<PathBuf, Vec<PathBuf>> {
    let mut found: Vec<PathBuf> = transcripts_under(source, dir)
        .into_iter()
        .filter(|p| !crate::harness::reads::is_hook_envelope(p))
        .collect();
    if found.len() == 1 {
        Ok(found.remove(0))
    } else {
        Err(found)
    }
}

/// Where every recording's CLI runs. The path is FIXED and generic on purpose:
/// every payload embeds its own cwd, the decoders read it, so the strip keeps it —
/// capturing somewhere already generic is what keeps a per-run path out of the
/// bytes. The recorder prepares it; a hermetic script names files under it.
///
/// # Errors
///
/// As [`capture_root`].
pub fn capture_workspace() -> std::io::Result<PathBuf> {
    Ok(capture_root()?.join(capture_workspace_tail()))
}

/// The directory capture workspaces sit under. On Windows that is ProgramData,
/// outside every user's profile: CC reads config from each ancestor of its cwd,
/// a home's `.claude` included.
///
/// # Errors
///
/// On Windows, if the shell has no ProgramData, as in an environment cleared
/// of the host's.
pub fn capture_root() -> std::io::Result<PathBuf> {
    #[cfg(windows)]
    let root = crate::harness::host::known_folder(crate::harness::host::KnownFolder::ProgramData)?;
    #[cfg(not(windows))]
    let root = PathBuf::from("/tmp");
    Ok(root)
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

/// How long a server the rig starts gets to come up.
pub const BOOT: Duration = Duration::from_secs(60);
/// How often a server that is coming up is asked whether it serves.
const POLL: Duration = Duration::from_millis(200);

/// Waits up to `within` for `ready` to answer true; an error it returns ends
/// the wait.
///
/// # Errors
///
/// `ready`'s, or that `what` never came up.
pub fn wait_until(
    what: &str,
    within: Duration,
    mut ready: impl FnMut() -> std::io::Result<bool>,
) -> std::io::Result<()> {
    let started = Instant::now();
    loop {
        if ready()? {
            return Ok(());
        }
        if started.elapsed() >= within {
            return Err(std::io::Error::other(format!("{what} never came up")));
        }
        std::thread::sleep(POLL);
    }
}

/// What the recorder writes into the [`NOTE_FILE`] of the workspace: what every
/// scripted turn reads, and what a script waits for before it replies.
pub const CAPTURE_NOTE: &str = "pong";

/// The file every capture workspace starts with, which the scripts read and a
/// permission script's refused call would delete.
pub const NOTE_FILE: &str = "NOTE.txt";

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
    /// One word of `Options::daemon`'s start.
    pub const DAEMON: &str = "--daemon";
    /// One word of `Options::daemon`'s readiness probe.
    pub const DAEMON_READY: &str = "--daemon-ready";
    /// One `KEY=VALUE` of `Options::daemon`'s own environment.
    pub const DAEMON_ENV: &str = "--daemon-env";
    /// One word of `Options::resume`, the invocation a second turn runs.
    pub const RESUME: &str = "--resume-word";
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
    /// The turn that resumed its session, for a run that had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed: Option<Resumed>,
    /// Whether the note the recorder planted was still there, unchanged, once
    /// every turn exited: a refused call that deletes it did not stay refused.
    #[serde(default)]
    pub note_survived: bool,
}

impl CaptureRecord {
    /// Whether the CLI ran `shape`'s turn as scripted, and the turn resuming
    /// it where one ran.
    pub fn exited_as_scripted(&self, shape: crate::harness::hermetic::Shape) -> bool {
        shape.exits_as_scripted(self.exit_code)
            && self
                .resumed
                .as_ref()
                .is_none_or(|r| shape.exits_as_scripted(r.exit_code))
    }
}

/// A second turn, as the recorder ran it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resumed {
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

    /// Its `provenance.json`.
    pub fn provenance_path(&self) -> PathBuf {
        self.dir.join(PROVENANCE)
    }
}

/// A file's lines that parse; the Drive reports the ones that don't.
pub(crate) fn json_lines(file: &Path) -> std::io::Result<Vec<serde_json::Value>> {
    Ok(std::fs::read_to_string(file)?
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
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

/// A kind of token a CLI or the recorder mints afresh each run.
struct IdFamily {
    /// The token's name in a canonical capture.
    name: &'static str,
    /// The token, or its one group where only its key tells the token from a
    /// value of the same shape.
    pattern: &'static str,
    /// A committed capture that carries one, under `tests/sources`.
    #[cfg_attr(not(test), expect(dead_code, reason = "its test reads the citation"))]
    seen_in: &'static str,
    /// Tokens of it two recordings of one turn minted, off the corpus's own
    /// re-records.
    #[cfg_attr(not(test), expect(dead_code, reason = "its test reads the pairs"))]
    renamed: &'static [(&'static str, &'static str)],
}

/// Every family of minted token a tier recording carries, so two recordings
/// of one turn differ in these alone. Where two match at once, the one that
/// starts first wins, then the earlier row.
const ID_FAMILIES: &[IdFamily] = &[
    // The time-ordered v7 codex and grok mint included.
    IdFamily {
        name: "uuid",
        pattern: r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
        seen_in: "fixtures/claude-code/tool-run-hermetic",
        renamed: &[(
            "3a94538f-09fe-4231-8195-8e5277ac55fe",
            "5c268a10-f98f-494a-b750-92a110ad30a9",
        )],
    },
    IdFamily {
        name: "anthropic-tool-use",
        pattern: r"toolu_[0-9A-Za-z_-]+",
        seen_in: "fixtures/claude-code/tool-run-hermetic",
        renamed: &[("toolu_whFwVaQVHr-tJpiG", "toolu_dft8jZ_PBInjS87R")],
    },
    // Not the wire names that hold it (`call_id`, `function_call_output`): a
    // minted id starts a word, and its tail outruns every wire name's.
    IdFamily {
        name: "openai-tool-call",
        pattern: r"\bcall_[0-9A-Za-z_-]{16,}",
        seen_in: "fixtures/kimi/tool-run-hermetic",
        renamed: &[
            ("call_orvKmWb1fXgIDLfw", "call_2Xs85giJlWCQx58K"),
            (
                "call_00_CsfLiRTSh5dPH4PAwUsZ5285",
                "call_00_DLuPvdg14pUXhocfirg30399",
            ),
        ],
    },
    IdFamily {
        name: "claude-code-subagent",
        pattern: r"\ba[0-9a-f]{16}\b",
        seen_in: "claude/fixtures/subagent-hermetic",
        renamed: &[("agent-a8074a27caeedb715", "agent-a615cb511b5899d39")],
    },
    IdFamily {
        name: "opencode-session",
        pattern: r"ses_[0-9A-Za-z]{26}",
        seen_in: "fixtures/opencode/tool-run-hermetic",
        renamed: &[(
            "ses_eabb87c3cffeVHXkPkR0sAM6MD",
            "ses_eab5376a1ffezRy0f5TzOmUH7a",
        )],
    },
    IdFamily {
        name: "reasonix-subagent",
        pattern: r"sa_\d{8}_\d{6}_\d{9}_[0-9a-f]{12}",
        seen_in: "reasonix/fixtures/subagent-hermetic",
        renamed: &[(
            "sa_20261019_162721_000000000_167006df8d3b",
            "sa_20261019_162729_000000000_53910816d8e4",
        )],
    },
    IdFamily {
        name: "reasonix-session-start",
        pattern: r"\d{8}-\d{6}\.\d{9}",
        seen_in: "fixtures/reasonix/tool-run-hermetic",
        renamed: &[(
            "20261019-063638.389045000-deepseek-v4-flash",
            "20261019-162745.781162000-deepseek-v4-flash",
        )],
    },
    IdFamily {
        name: "hermes-session",
        pattern: r"\d{8}_\d{6}_[0-9a-f]{6}",
        seen_in: "fixtures/hermes/tool-run-hermetic",
        renamed: &[("20261020_005102_5e501b", "20261020_005045_5564c8")],
    },
    // Codex's and omp's.
    IdFamily {
        name: "session-file-start",
        pattern: r"\d{4}-\d\d-\d\dT\d\d-\d\d-\d\d(?:-\d{3}Z)?",
        seen_in: "codex/fixtures/subagent-hermetic",
        renamed: &[
            (
                "rollout-2026-10-18T22-11-05-x",
                "rollout-2026-10-20T00-47-42-x",
            ),
            ("2026-10-19T22-12-48-510Z_x", "2026-10-20T07-52-32-773Z_x"),
        ],
    },
    IdFamily {
        name: "recorder-sandbox",
        pattern: r"\.tmp[0-9A-Za-z]{6}",
        seen_in: "delegation/fixtures/omp",
        renamed: &[("/T/.tmpbelEXY/homes", "/T/.tmpLZD2ds/homes")],
    },
    IdFamily {
        name: "shim-pid",
        pattern: r#""_pid":(\d+)"#,
        seen_in: "fixtures/dsh/tool-run-recorded",
        renamed: &[(r#"{"_pid":56875}"#, r#"{"_pid":42895}"#)],
    },
    IdFamily {
        name: "shim-clock",
        pattern: r#""_shim_ts_ms":(\d+)"#,
        seen_in: "fixtures/dsh/tool-run-recorded",
        renamed: &[(
            r#"{"_shim_ts_ms":1792464600287}"#,
            r#"{"_shim_ts_ms":1792482405228}"#,
        )],
    },
    // A port the orchestrator picks free.
    IdFamily {
        name: "openclaw-gateway-port",
        pattern: r#""gatewayPort":(\d+)"#,
        seen_in: "fixtures/openclaw/tool-run-hermetic",
        renamed: &[(r#"{"gatewayPort":55736}"#, r#"{"gatewayPort":63297}"#)],
    },
];

fn id_patterns() -> std::io::Result<Vec<regex::Regex>> {
    ID_FAMILIES
        .iter()
        .map(|f| regex::Regex::new(f.pattern).map_err(std::io::Error::other))
        .collect()
}

/// `text` with each of its minted ids replaced by what `name` makes of its
/// family's row and the token.
fn with_ids_named(
    text: &str,
    patterns: &[regex::Regex],
    mut name: impl FnMut(usize, &str) -> String,
) -> String {
    let mut found: Vec<(usize, usize, usize)> = patterns
        .iter()
        .enumerate()
        .flat_map(|(row, re)| {
            re.captures_iter(text).filter_map(move |c| {
                let token = c.get(1).or_else(|| c.get(0))?;
                Some((token.start(), row, token.end()))
            })
        })
        .collect();
    found.sort_unstable();
    let (mut out, mut at) = (String::new(), 0);
    for (start, row, end) in found {
        if start < at {
            continue;
        }
        out.push_str(&text[at..start]);
        out.push_str(&name(row, &text[start..end]));
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

/// A capture's files up to the ids its run minted: each file's path from
/// `dir` and its text, every `ID_FAMILIES` token in either replaced by
/// `⟨name#n⟩`, `n` its family's order of first appearance across the capture,
/// so one numbering is the one bijection two recordings must share. Files are
/// walked in the order of their paths, then their texts, with ids masked, as
/// a raw sort would order two id-named session dirs at random; none when two
/// files tie there, as only their ids tell them apart.
///
/// # Errors
///
/// If a file cannot be read, or a family's pattern does not compile.
pub fn canonical_capture(dir: &Path) -> std::io::Result<Option<Vec<(String, String)>>> {
    let patterns = id_patterns()?;
    let masked = |text: &str| {
        with_ids_named(text, &patterns, |row, _| {
            format!("⟨{}⟩", ID_FAMILIES[row].name)
        })
    };
    let mut files = Vec::new();
    for file in jsonl_files_under(dir) {
        let path = slashed_under(dir, &file)?;
        let text = std::fs::read_to_string(&file)?;
        files.push(((masked(&path), masked(&text)), (path, text)));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    if files.windows(2).any(|w| w[0].0 == w[1].0) {
        return Ok(None);
    }
    let mut seen: Vec<std::collections::HashMap<String, usize>> =
        ID_FAMILIES.iter().map(|_| Default::default()).collect();
    let mut numbered = |text: &str| {
        with_ids_named(text, &patterns, |row, token| {
            let next = seen[row].len();
            let n = *seen[row].entry(token.to_owned()).or_insert(next);
            format!("⟨{}#{n}⟩", ID_FAMILIES[row].name)
        })
    };
    Ok(Some(
        files
            .into_iter()
            .map(|(_, (path, text))| (numbered(&path), numbered(&text)))
            .collect(),
    ))
}

/// `path` from `base`, its components `/`-joined as every host's checkout
/// spells them.
///
/// # Errors
///
/// Where `path` is not under `base`: no spelling of it is the checkout's.
pub fn slashed_under(base: &Path, path: &Path) -> std::io::Result<String> {
    let under = path.strip_prefix(base).map_err(|_| {
        std::io::Error::other(format!(
            "{} is not under {}",
            path.display(),
            base.display()
        ))
    })?;
    Ok(under
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

/// THE enumeration of committed captures under a `tests/sources` tree, from the
/// layout alone: the provenance gates walk it and the recorder re-strips it, so
/// a fix lands on the whole population. Three shapes: `fixtures/<source>/<scenario>/`;
/// `<module>/fixtures/` declaring at its root; `<module>/fixtures/<sub>/`, owned by
/// the module when its `MODULE_TO_SOURCE` id is registered (omp's, claude's), else
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
        let module_source = source_id(&module_name);
        let module_is_source = registry::descriptor_for(&module_source).is_some();
        out.extend(subs.into_iter().map(|sub| Capture {
            source: if module_is_source {
                module_source.clone()
            } else {
                source_id(&name(&sub))
            },
            dir: sub,
        }));
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A capture of `files`, each `(path, text)`, read canonical.
    fn canonical(files: &[(&str, &str)]) -> Option<Vec<(String, String)>> {
        let d = tempfile::tempdir().expect("tempdir");
        for (path, text) in files {
            let to = d.path().join(path);
            std::fs::create_dir_all(to.parent().expect("parent")).expect("mkdir");
            std::fs::write(to, text).expect("write");
        }
        canonical_capture(d.path()).expect("readable")
    }

    fn same_up_to_ids(old: &[(&str, &str)], new: &[(&str, &str)]) -> bool {
        let old = canonical(old);
        old.is_some() && old == canonical(new)
    }

    fn refs(files: &[(String, String)]) -> Vec<(&str, &str)> {
        files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect()
    }

    const U1: &str = "3e335f66-3b4f-473b-abf2-5b7cb52d7a0d";
    const U2: &str = "4f446a77-4c5a-584c-bca3-6c8dc63e8b1e";
    const U3: &str = "5a557b88-5d6b-695d-cdb4-7d9ed74f9c2f";

    #[test]
    fn a_file_name_renames_with_the_ids_inside_it() {
        let file = |name: &str, holds: &str| {
            vec![(format!("s/{name}.jsonl"), format!(r#"{{"id":"{holds}"}}"#))]
        };
        let old = file(U1, U1);
        assert!(same_up_to_ids(&refs(&old), &refs(&file(U2, U2))));
        assert!(
            !same_up_to_ids(&refs(&old), &refs(&file(U2, U3))),
            "the file is named for one session and holds another"
        );
    }

    #[test]
    fn two_ids_renamed_to_one_or_one_to_two_is_no_bijection() {
        let two = format!(r#"{{"a":"{U1}","b":"{U2}"}}"#);
        let one = format!(r#"{{"a":"{U3}","b":"{U3}"}}"#);
        assert!(!same_up_to_ids(&[("h.jsonl", &two)], &[("h.jsonl", &one)]));
        assert!(!same_up_to_ids(&[("h.jsonl", &one)], &[("h.jsonl", &two)]));
        let swapped = format!(r#"{{"a":"{U3}","b":"{U1}"}}"#);
        assert!(
            same_up_to_ids(&[("h.jsonl", &two)], &[("h.jsonl", &swapped)]),
            "U2 may take U1's old name"
        );
    }

    #[test]
    fn a_two_session_capture_whose_dirs_swap_raw_order_reads_the_same() {
        let dir = crate::harness::sources_root().join("grok/fixtures/subagent-hermetic");
        let old: Vec<(String, String)> = jsonl_files_under(&dir)
            .into_iter()
            .map(|f| {
                let path = f.strip_prefix(&dir).expect("under").to_string_lossy();
                let text = std::fs::read_to_string(&f).expect("read");
                (path.replace('\\', "/"), text)
            })
            .collect();
        let mut sessions: Vec<&str> = old
            .iter()
            .filter_map(|(path, _)| path.split_once('/').map(|(session, _)| session))
            .collect();
        sessions.dedup();
        assert!(
            sessions.len() >= 2,
            "a parent and a child session: {sessions:?}"
        );
        // The rename that sorts the dirs in reverse: the first takes the last name.
        let renamed = |text: &str| {
            sessions
                .iter()
                .enumerate()
                .fold(text.to_owned(), |text, (i, id)| {
                    text.replace(
                        id,
                        &format!("{:08x}-0000-4000-8000-000000000000", sessions.len() - i),
                    )
                })
        };
        let new: Vec<(String, String)> = old
            .iter()
            .map(|(path, text)| (renamed(path), renamed(text)))
            .collect();
        assert!(same_up_to_ids(&refs(&old), &refs(&new)));
    }

    #[test]
    fn files_only_their_ids_tell_apart_have_no_canonical_form() {
        let a = format!("{U1}.jsonl");
        let b = format!("{U2}.jsonl");
        assert_eq!(canonical(&[(&a, "{}"), (&b, "{}")]), None);
        assert!(
            canonical(&[(&a, r#"{"role":"parent"}"#), (&b, r#"{"role":"child"}"#)]).is_some(),
            "their texts tell them apart"
        );
    }

    #[test]
    fn a_file_more_or_a_text_changed_is_another_capture() {
        let old = [("h.jsonl", r#"{"model":"deepseek-flash"}"#)];
        assert!(!same_up_to_ids(&old, &[old[0], ("z.jsonl", "{}")]));
        assert!(
            !same_up_to_ids(&old, &[("h.jsonl", r#"{"model":"deepseek-fresh"}"#)]),
            "text no family names, even renamed one for one"
        );
        assert!(
            !same_up_to_ids(
                &[("h.jsonl", r#"{"sessionId":"session-q7Hk29Lm4QpZ"}"#)],
                &[("h.jsonl", r#"{"sessionId":"session-r8Il30Mn5RqA"}"#)]
            ),
            "an id-like token outside the table"
        );
    }

    #[test]
    fn a_token_one_family_starts_wins_over_another_nested_in_it() {
        assert!(same_up_to_ids(
            &[(
                "h.jsonl",
                r#"{"subagentId":"sa_20261019_162721_000000000_167006df8d3b"}"#
            )],
            &[(
                "h.jsonl",
                r#"{"subagentId":"sa_20261020_101010_000000000_aaaaaaaaaaaa"}"#
            )]
        ));
    }

    #[test]
    fn a_wire_name_that_holds_call_is_text_not_an_id() {
        for name in [
            "call_id",
            "tool_call_id",
            "tool_call_update",
            "function_call_output",
            "custom_tool_call_output",
        ] {
            let was = format!(r#"{{"{name}":1}}"#);
            let now = format!(r#"{{"{}x":1}}"#, &name[..name.len() - 1]);
            assert!(
                !same_up_to_ids(&[("h.jsonl", &was)], &[("h.jsonl", &now)]),
                "{name}"
            );
        }
        assert!(
            !same_up_to_ids(
                &[("h.jsonl", r#"{"tool":"recall_0123456789abcdef"}"#)],
                &[("h.jsonl", r#"{"tool":"recall_0123456789abcdex"}"#)]
            ),
            "a word that holds call_ is no id"
        );
    }

    #[test]
    fn each_family_renames_what_two_recordings_minted() {
        let patterns = id_patterns().expect("every pattern compiles");
        for (family, pattern) in ID_FAMILIES.iter().zip(&patterns) {
            assert!(!family.renamed.is_empty(), "{}: no pair", family.name);
            for (was, now) in family.renamed {
                assert!(
                    pattern.is_match(was) && pattern.is_match(now),
                    "{}: {was} or {now} is not its token",
                    family.name
                );
                assert!(
                    same_up_to_ids(&[("h.jsonl", was)], &[("h.jsonl", now)]),
                    "{}: {was} -> {now}",
                    family.name
                );
            }
        }
    }

    #[test]
    fn every_id_family_is_carried_by_the_capture_it_cites() {
        let patterns = id_patterns().expect("every pattern compiles");
        for (family, pattern) in ID_FAMILIES.iter().zip(&patterns) {
            let capture = crate::harness::sources_root().join(family.seen_in);
            let carried = jsonl_files_under(&capture).iter().any(|f| {
                pattern.is_match(&slashed_under(&capture, f).expect("under"))
                    || pattern.is_match(&std::fs::read_to_string(f).expect("read"))
            });
            assert!(carried, "{}: {} carries none", family.name, family.seen_in);
        }
    }

    #[test]
    fn a_path_under_its_base_is_slash_joined_and_one_elsewhere_has_no_spelling() {
        let base = Path::new("root");
        assert_eq!(
            slashed_under(base, &base.join("s").join("h.jsonl")).expect("under"),
            "s/h.jsonl"
        );
        assert!(slashed_under(base, Path::new("elsewhere/h.jsonl")).is_err());
    }

    #[test]
    fn a_wait_ends_on_ready_on_its_error_or_past_its_budget() {
        let mut asked = 0;
        let ready = wait_until("x", BOOT, || {
            asked += 1;
            Ok(asked == 2)
        });
        assert!(ready.is_ok() && asked == 2);
        let failed = wait_until("x", BOOT, || Err(std::io::Error::other("exited")));
        assert_eq!(failed.expect_err("its error").to_string(), "exited");
        let late = wait_until("x", Duration::ZERO, || Ok(false));
        assert_eq!(late.expect_err("past").to_string(), "x never came up");
    }

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

    #[test]
    fn a_capture_has_one_transcript_or_says_what_it_found() {
        // codex admits every path, so only the envelope rule keeps its hooks out.
        let d = tempfile::tempdir().expect("tempdir");
        assert_eq!(transcript_in("codex", d.path()), Err(vec![]));
        let rollout = d.path().join("rollout.jsonl");
        std::fs::write(&rollout, "{}").expect("write");
        std::fs::write(d.path().join(HOOK_PAYLOADS), "{}").expect("write");
        assert_eq!(transcript_in("codex", d.path()), Ok(rollout.clone()));
        std::fs::write(d.path().join("rollout-stale.jsonl"), "{}").expect("write");
        let stale = transcript_in("codex", d.path()).expect_err("two transcripts");
        assert_eq!(stale.len(), 2, "{stale:?}");
        assert!(stale.contains(&rollout));
    }
}
