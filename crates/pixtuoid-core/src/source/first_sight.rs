//! A transcript's first sight: the head scan, the `SessionStart` the JSONL
//! watcher registers it with, and the lifecycle verdict on when it registers
//! again, which the harness's seed runs through the same code. A wasm-clean
//! build has neither caller.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::AgentId;
use crate::source::AgentEvent;
use crate::source::decoder::{CwdExtractor, ParentExtractor, SUBAGENTS_DIR};

/// How much of a transcript's head a first sight reads, so registration never
/// reads a whole multi-MB transcript.
pub(crate) const HEAD_BYTES: usize = 1 << 20;

/// What a transcript's first sight still holds. Absent = never registered, or
/// fully retired by an exit un-claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sight {
    /// Registered as its reads last resolved it: a read [`Sight::registers`]
    /// admits re-registers it, so a slot the reducer forgot comes back.
    Claimed(Registration),
    /// Its session ended: the last lifecycle evidence of a read or of an
    /// oversized span's tail is its end, or a child-end un-claim. The path
    /// stays KNOWN, so the watcher's re-vouch won't replay it however live the
    /// probe says the (still-open) rollout is.
    Ended,
}

impl Sight {
    /// Whether a read after `prev` registers its session: a first sight does,
    /// a seen one only when the read `lives`, so a trailer flushed after its end
    /// line (grok's `hook_execution{stop}`) registers nothing; and no read whose
    /// `verdict` is its end does, or its sprite walks in only to walk out.
    pub(crate) fn registers(prev: Option<&Self>, lives: bool, verdict: Option<Life>) -> bool {
        verdict != Some(Life::Ended) && (prev.is_none() || lives)
    }
}

/// What a path's reads resolved of its session, kept so a re-registration
/// reads no head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Registration {
    /// The [`first_sight_start`] its first sight built.
    pub(crate) start: AgentEvent,
    /// The label its reads last gave the session: first sight's, then any
    /// `Rename` a read decoded for it since. `None` where no label deriver is
    /// built: the harness, as [`HeadLabel`]'s native-only column.
    pub(crate) label: Option<String>,
}

/// What a decoded event says of its transcript's session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Life {
    /// A start, or work ([`AgentEvent::is_work`]), whoever it names.
    Lives,
    /// An end of the path's own agent: a child's end in its parent's
    /// transcript ends only the child.
    Ended,
}

fn evidence(ev: &AgentEvent, own: AgentId) -> Option<Life> {
    match ev {
        AgentEvent::SessionEnd { .. } if ev.agent_id() == own => Some(Life::Ended),
        AgentEvent::SessionStart { .. } => Some(Life::Lives),
        _ => ev.is_work().then_some(Life::Lives),
    }
}

/// The session's verdict over `events`, in order: its last evidence, so a start
/// or work after an end is a resume.
pub(crate) fn last_life<'a>(
    events: impl IntoIterator<Item = &'a AgentEvent>,
    own: AgentId,
) -> Option<Life> {
    events.into_iter().filter_map(|ev| evidence(ev, own)).last()
}

/// Whether `events` hold any [`Life::Lives`] evidence: the life a read shows
/// whose whole span it then sends, a trailing end included.
pub(crate) fn holds_life<'a>(
    events: impl IntoIterator<Item = &'a AgentEvent>,
    own: AgentId,
) -> bool {
    events
        .into_iter()
        .any(|ev| evidence(ev, own) == Some(Life::Lives))
}

/// Whether a read whose verdict is `verdict` sends its decoded `event`: not its
/// own `SessionStart` when the read ends the session, a start a decoder bears
/// (copilot's `session.resume`) refused as [`Sight::registers`] refuses the
/// watcher's.
pub(crate) fn sends(event: &AgentEvent, own: AgentId, verdict: Option<Life>) -> bool {
    verdict != Some(Life::Ended)
        || !matches!(event, AgentEvent::SessionStart { .. })
        || event.agent_id() != own
}

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
/// a [`HEAD_BYTES`] head once per field is a real cost. Each comes from the SCANNED
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

impl HeadFields {
    /// Whether a read that need not start the file misses the head's cwd, or its
    /// parent where the source extracts one, so the head is read instead: a
    /// revive's tail may lack Codex's `session_meta` cwd and parent, which would
    /// register it empty-cwd onto the short reap, or a child as a root.
    pub(crate) fn lack_the_head(&self, parent: Option<ParentExtractor>) -> bool {
        self.cwd.is_none() || (parent.is_some() && self.parent.is_none())
    }
}

/// The `SessionStart` a transcript's first sight registers, keyed by
/// `session_id`; its `subagents/` nesting names the parent before its head does.
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
    use crate::source::decoder::is_subagent_path;
    use crate::source::registry::cwd_extractor_for;

    /// The registration gate: a first sight or a read that lives registers, and
    /// a read whose verdict is its end registers nothing, seen or not.
    #[test]
    fn a_read_that_ends_its_session_registers_nothing() {
        let rows = [
            (None, false, None, true),
            (None, true, Some(Life::Lives), true),
            (None, true, Some(Life::Ended), false),
            (Some(Sight::Ended), false, None, false),
            (Some(Sight::Ended), true, Some(Life::Lives), true),
            (Some(Sight::Ended), true, Some(Life::Ended), false),
        ];
        for (prev, lives, verdict, registers) in rows {
            assert_eq!(
                Sight::registers(prev.as_ref(), lives, verdict),
                registers,
                "{prev:?} {lives} {verdict:?}"
            );
        }
    }

    /// The lifecycle fold: the last evidence is the verdict, a start or activity is
    /// life whoever it names, and only the path's own end is an end.
    #[test]
    fn the_last_lifecycle_evidence_is_the_verdict() {
        let own = AgentId::from_parts("test", "own");
        let child = AgentId::from_parts("test", "child");
        let start = AgentEvent::SessionStart {
            agent_id: own,
            source: "test".into(),
            session_id: "own".into(),
            cwd: PathBuf::from("/repo"),
            parent_id: None,
        };
        let end = |agent_id| AgentEvent::SessionEnd {
            agent_id,
            as_child: false,
        };
        let call = AgentEvent::tool_use_start(own, None, None);
        let usage = AgentEvent::usage(own, 1, None);
        let rows: [(&[AgentEvent], Option<Life>); 8] = [
            (&[], None),
            (std::slice::from_ref(&start), Some(Life::Lives)),
            (&[start.clone(), end(own)], Some(Life::Ended)),
            (&[end(own), start.clone()], Some(Life::Lives)),
            (&[end(own), call], Some(Life::Lives)),
            (&[end(own), usage], Some(Life::Ended)),
            (&[end(child)], None),
            (&[end(own), end(child)], Some(Life::Ended)),
        ];
        for (events, verdict) in rows {
            assert_eq!(last_life(events, own), verdict, "{events:?}");
        }
    }

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

    /// A cwd whose closing brace is the cap's last byte is read; one whose brace
    /// is the first byte past it is not, whatever the caller hands in.
    #[test]
    fn the_head_scan_reads_no_further_than_its_cap() {
        let obj = "{\"cwd\":\"/repo\"}";
        let after = |pad: usize| format!("{}\n{obj}", "x".repeat(pad));
        let cwd = |text: String| {
            let extract = crate::source::decoder::extract_top_level_cwd;
            extract_head_fields(text.as_bytes(), extract, None, None).cwd
        };
        assert!(cwd(after(HEAD_BYTES - 1 - obj.len())).is_some());
        assert!(cwd(after(HEAD_BYTES - obj.len())).is_none());
    }

    #[test]
    fn detect_parent_id_derives_grandparent_transcript_key() {
        let parent: PathBuf = ["projects", "x", "abc123"].iter().collect();
        let p = parent.join("subagents").join("agent-1.jsonl");
        let expected = AgentId::from_parts("claude-code", "abc123");
        assert_eq!(detect_parent_id(&p, "claude-code"), Some(expected));
        assert!(is_subagent_path(&p));
    }

    #[test]
    fn detect_parent_id_none_for_regular_and_lookalike_paths() {
        assert_eq!(
            detect_parent_id(
                Path::new("/Users/me/.claude/projects/x/ses.jsonl"),
                "claude-code"
            ),
            None
        );
        let lookalike = Path::new("/Users/me/.claude/projects/subagents-paper/ses.jsonl");
        assert_eq!(detect_parent_id(lookalike, "claude-code"), None);
        assert!(!is_subagent_path(lookalike));
        assert_eq!(
            detect_parent_id(Path::new("subagents/agent-1.jsonl"), "claude-code"),
            None
        );
    }

    #[test]
    fn detect_parent_id_keys_on_parent_uuid_component() {
        let sub =
            Path::new("/Users/me/.claude/projects/-Users-me-proj/abc123/subagents/agent-1.jsonl");
        let expected = AgentId::from_parts("claude-code", "abc123");
        assert_eq!(detect_parent_id(sub, "claude-code"), Some(expected));
    }

    #[test]
    fn detect_parent_id_survives_cwd_split() {
        let under_a =
            Path::new("/Users/me/.claude/projects/-PROJECT-A/abc123/subagents/agent-1.jsonl");
        let under_b =
            Path::new("/Users/me/.claude/projects/-PROJECT-B/abc123/subagents/agent-1.jsonl");
        let expected = AgentId::from_parts("claude-code", "abc123");
        assert_eq!(detect_parent_id(under_a, "claude-code"), Some(expected));
        assert_eq!(detect_parent_id(under_b, "claude-code"), Some(expected));
        assert_eq!(
            detect_parent_id(under_a, "claude-code"),
            detect_parent_id(under_b, "claude-code"),
            "same <parent-uuid> under different project dirs resolves to the same parent"
        );
    }

    #[test]
    fn detect_parent_id_handles_workflow_nesting() {
        let sub = Path::new(
            "/Users/me/.claude/projects/p/abc123/subagents/workflows/wf_0d/agent-y.jsonl",
        );
        let expected = AgentId::from_parts("claude-code", "abc123");
        assert_eq!(detect_parent_id(sub, "claude-code"), Some(expected));
    }

    #[cfg(windows)]
    #[test]
    fn detect_parent_id_handles_backslash_paths() {
        let p = Path::new(r"C:\Users\Me\.claude\projects\x\abc123\subagents\agent-1.jsonl");
        let expected = AgentId::from_parts("claude-code", "abc123");
        assert_eq!(detect_parent_id(p, "claude-code"), Some(expected));
        assert!(is_subagent_path(p));
    }

    #[test]
    fn the_head_scan_dispatches_the_scanned_sources_shape() {
        let top = br#"{"cwd":"/repo/a"}"#;
        assert_eq!(
            extract_head_fields(top, cwd_extractor_for("omp"), None, None).cwd,
            Some(PathBuf::from("/repo/a"))
        );
        assert_eq!(
            extract_head_fields(top, cwd_extractor_for("test"), None, None).cwd,
            Some(PathBuf::from("/repo/a")),
            "an unregistered harness source keeps the shared top-level default"
        );
        let nested = br#"{"type":"session_meta","payload":{"cwd":"/repo/b","id":"u"}}"#;
        assert_eq!(
            extract_head_fields(nested, cwd_extractor_for("codex"), None, None).cwd,
            Some(PathBuf::from("/repo/b"))
        );
        let mixed = b"not-json\n{\"type\":\"noise\"}\n{\"cwd\":\"/repo/c\"}\n";
        assert_eq!(
            extract_head_fields(mixed, cwd_extractor_for("omp"), None, None).cwd,
            Some(PathBuf::from("/repo/c"))
        );
    }

    #[test]
    fn cc_head_scan_ignores_codex_shaped_payload_cwd() {
        let codex_shaped = br#"{"type":"session_meta","payload":{"cwd":"/foreign/repo","id":"u"}}"#;
        assert_eq!(
            extract_head_fields(codex_shaped, cwd_extractor_for("claude-code"), None, None).cwd,
            None,
            "a foreign source's cwd shape must not extract for a CC transcript"
        );
    }

    /// A stand-in for omp's line-1 title slot: names the session from the head.
    fn t_head_label(v: &serde_json::Value) -> Option<String> {
        let t = v.get("title")?.as_str()?;
        (!t.is_empty()).then(|| format!("t·{t}"))
    }

    /// Both head fields come back from ONE pass, and a source with no head label
    /// still gets its cwd. (The scan's STOP point — the cost half of this seam — is
    /// pinned by `head_scan_stops_on_cwd_alone_when_the_source_has_no_head_label`,
    /// where the predicate is directly observable.)
    #[test]
    fn a_head_read_yields_cwd_and_the_optional_title_in_one_pass() {
        let plain = "{\"type\":\"session\",\"cwd\":\"/repo/head\"}\n{\"type\":\"assistant\"}\n";
        let head = extract_head_fields(plain.as_bytes(), cwd_extractor_for("test"), None, None);
        assert_eq!(head.cwd.as_deref(), Some(Path::new("/repo/head")));
        assert_eq!(
            head.label, None,
            "a source with no head label must name nothing"
        );

        let titled = "{\"type\":\"title\",\"title\":\"Ship it\"}\n{\"type\":\"session\",\"cwd\":\"/repo/head\"}\n";
        let head = extract_head_fields(
            titled.as_bytes(),
            cwd_extractor_for("test"),
            Some(t_head_label),
            None,
        );
        assert_eq!(head.cwd.as_deref(), Some(Path::new("/repo/head")));
        assert_eq!(head.label.as_deref(), Some("t·Ship it"));

        // The title slot exists but is EMPTY (omp's subagents, forever): the head
        // read must fall through to the cwd-basename deriver, not name it "".
        let empty =
            "{\"type\":\"title\",\"title\":\"\"}\n{\"type\":\"session\",\"cwd\":\"/repo/head\"}\n";
        let head = extract_head_fields(
            empty.as_bytes(),
            cwd_extractor_for("test"),
            Some(t_head_label),
            None,
        );
        assert_eq!(
            head.label, None,
            "an empty slot must not become an empty name"
        );
    }
}
