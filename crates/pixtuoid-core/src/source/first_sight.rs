//! A transcript's first sight: the head scan and the `SessionStart` the JSONL
//! watcher registers it with, which the harness's seed builds through the same
//! code. A wasm-clean build has neither caller.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::AgentId;
use crate::source::AgentEvent;
use crate::source::decoder::{CwdExtractor, ParentExtractor, SUBAGENTS_DIR};

/// How much of a transcript's head a first sight reads, so registration never
/// reads a whole multi-MB transcript.
pub(crate) const HEAD_BYTES: usize = 1 << 20;

/// Detect a CC subagent by the `subagents` path component and link it to its
/// parent via the directory component immediately before it (`<parent-uuid>`).
/// That UUID equals the parent's own id, so the link resolves even when the
/// subagent transcript lands under a DIFFERENT project dir than the parent (a
/// git-worktree cwd-split). CC-layout-specific.
pub(crate) fn detect_parent_id(path: &Path, source: &str) -> Option<AgentId> {
    let mut prev: Option<&str> = None;
    for c in path.components() {
        if c.as_os_str() == SUBAGENTS_DIR {
            return prev.map(|uuid| AgentId::from_parts(source, uuid));
        }
        prev = c.as_os_str().to_str();
    }
    None
}

/// Pulls a display label out of ONE decoded head line, for a source that names
/// its session in the transcript HEAD rather than only in the tail stream.
/// `None` on a source's registry row means it has no such name — which is
/// load-bearing, not merely absent: it lets the head scan stop at the first
/// `cwd` instead of reading on for a label that can never arrive. **omp** is
/// the one override; see its `omp_head_title`.
pub(crate) type HeadLabel = fn(&Value) -> Option<String>;

/// Walk a byte span's JSONL lines, handing each parsed line to `visit` until it
/// returns `true`. A line that does not parse is skipped.
fn scan_jsonl(bytes: &[u8], mut visit: impl FnMut(&Value) -> bool) {
    let parsed = crate::source::admit::lines(bytes)
        .filter_map(|line| serde_json::from_str::<Value>(line.ok()?).ok());
    for v in parsed {
        if visit(&v) {
            return;
        }
    }
}

/// Whether a head scan can stop: everything it is ABLE to find, it has found.
///
/// `wants_label` is the whole reason `head_label` is an `Option`. A source with
/// no head name can never satisfy a label term, so folding one in unconditionally
/// turns its first-line stop into a full parse up to [`HEAD_BYTES`] — on a path
/// every transcript source rides.
fn head_scan_complete(cwd: Option<&Path>, label: Option<&str>, wants_label: bool) -> bool {
    cwd.is_some() && (!wants_label || label.is_some())
}

/// The first-sight fields a HEAD read yields, pulled in ONE parse pass: scanning
/// a 1 MiB head once per field is a real cost. Each comes from the SCANNED
/// source's own extractor (invariant #3): an if-chain trying every source's
/// shape against every transcript lets a foreign-shaped line label a session
/// with a foreign, identity-bearing cwd. For a TITLED omp root — the only
/// source with a head label — the two land on adjacent lines (`title`, then
/// `session`) and the scan stops there. A title-less head (every subagent, and
/// legacy files with no slot) has no label to find, so it scans to
/// [`HEAD_BYTES`], whoever passes more.
pub(crate) fn extract_head_fields(
    bytes: &[u8],
    extract: CwdExtractor,
    head_label: Option<HeadLabel>,
    head_parent: Option<ParentExtractor>,
) -> HeadFields {
    let mut head = HeadFields::default();
    scan_jsonl(bytes.get(..HEAD_BYTES).unwrap_or(bytes), |v| {
        if head.cwd.is_none() {
            head.cwd = extract(v);
        }
        if let (Some(f), None) = (head_label, &head.label) {
            head.label = f(v);
        }
        if let (Some(f), None) = (head_parent, &head.parent) {
            head.parent = f(v);
        }
        head_scan_complete(
            head.cwd.as_deref(),
            head.label.as_deref(),
            head_label.is_some(),
        )
    });
    head
}

/// What a first sight reads off a transcript's head, or off a revived file's
/// new bytes.
#[derive(Debug, Default)]
pub(crate) struct HeadFields {
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) label: Option<String>,
    pub(crate) parent: Option<String>,
}

/// The `SessionStart` a transcript's first sight registers, keyed by
/// `session_id` — its id deriver's output over the folded path: hook slots carry
/// the bare session UUID and `backfill_identity` never heals a non-empty
/// session_id, so a raw file-stem here disagrees with the hook-created twin
/// forever. Its `subagents/` nesting names the parent before its head does.
pub(crate) fn first_sight_start(
    path: &Path,
    source: &str,
    session_id: String,
    cwd: PathBuf,
    head_parent: Option<String>,
) -> AgentEvent {
    AgentEvent::SessionStart {
        agent_id: AgentId::from_parts(source, &session_id),
        source: source.to_string(),
        session_id,
        cwd,
        parent_id: detect_parent_id(path, source)
            .or_else(|| head_parent.map(|p| AgentId::from_parts(source, &p))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The truth table the scan's cost depends on. The `wants_label = false`
    /// row is load-bearing: it is the difference between stopping on the first
    /// `cwd` line and parsing the entire head.
    #[test]
    fn head_scan_stops_on_cwd_alone_when_the_source_has_no_head_label() {
        let cwd = Path::new("/repo");

        assert!(head_scan_complete(Some(cwd), None, false));
        assert!(head_scan_complete(Some(cwd), Some("t"), false));
        assert!(!head_scan_complete(None, Some("t"), false));

        assert!(!head_scan_complete(Some(cwd), None, true));
        assert!(head_scan_complete(Some(cwd), Some("t"), true));
        assert!(!head_scan_complete(None, None, true));
    }

    /// A cwd ending on the cap's last byte is read; one starting past it is not,
    /// whatever the caller hands in.
    #[test]
    fn the_head_scan_reads_no_further_than_its_cap() {
        let line = "{\"cwd\":\"/repo\"}\n";
        let after = |pad: usize| format!("{}\n{line}", "x".repeat(pad));
        let cwd = |text: String| {
            let extract = crate::source::decoder::extract_top_level_cwd;
            extract_head_fields(text.as_bytes(), extract, None, None).cwd
        };
        assert!(cwd(after(HEAD_BYTES - 1 - line.len())).is_some());
        assert!(cwd(after(HEAD_BYTES - 1)).is_none());
    }
}
