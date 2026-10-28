//! What a decoder READS of a capture, derived by probe and never listed: a
//! subtree is unread when neutralizing it leaves everything the registry derives
//! from the line unchanged. The recorder strips captures with it, so a field a
//! decoder stopped reading cannot linger in committed bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::provenance::{Deidentified, Provenance};
use super::{Capture, Drive, LineFailure, captures};
use crate::source::daemon::DecodedPresence;
use crate::source::registry;

/// One strip pass: how many subtrees were blanked, which files were left as
/// recorded because a line in them is not JSON, and which blank leaves the
/// decoders read.
#[derive(Debug, Default)]
pub struct Strip {
    /// Subtrees blanked across every file.
    pub blanked: usize,
    /// Files left whole, each with its first parse error: half-rewriting one
    /// would corrupt the bytes. The committed-tree gate refuses to skip such a
    /// line at all (`no_identity_key_holds_a_value`).
    pub not_json: Vec<(PathBuf, String)>,
    /// Each stripped file's blank leaves the decoders read once set, as
    /// `<line>:<pointer>`: the view its record keeps.
    pub sentinel_reads: BTreeMap<PathBuf, BTreeSet<String>>,
}

/// Blank every subtree no decoder reads, so an operator's roster, instructions
/// and paths leave without anyone naming them. DERIVED, never listed — a subtree
/// is blanked when blanking it leaves the decoded events identical — because a
/// hand-kept list drifts in the one direction nothing catches: a field the
/// decoder stopped reading stays in the bytes. Top-down, so an unread container
/// collapses whole, element count included.
///
/// # Errors
///
/// If a file cannot be read or rewritten.
pub fn strip_unread(source: &str, files: &[PathBuf]) -> std::io::Result<Strip> {
    let mut strip = Strip::default();
    for file in files {
        let Some(drive) = drive_for(source, file) else {
            continue;
        };
        let (raw, mut lines) = match parsed_lines(file)? {
            Ok(parsed) => parsed,
            Err(e) => {
                strip.not_json.push((file.clone(), e.to_string()));
                continue;
            }
        };
        let per_line = strip_with(source, &drive, file, &mut lines);
        strip.sentinel_reads.insert(
            file.clone(),
            sentinel_reads_of(source, &drive, &mut lines, is_hook_envelope(file)),
        );
        let mut out = String::new();
        for (i, here) in per_line.into_iter().enumerate() {
            strip.blanked += here;
            if here == 0 {
                out.push_str(&raw[i]);
            } else {
                out.push_str(&serde_json::to_string(&lines[i])?);
            }
            out.push('\n');
        }
        replace_atomically(file, out.as_bytes())?;
    }
    Ok(strip)
}

/// `file`'s `lines` blanked as [`strip_unread`] blanks them; `false` when no
/// decoder reads `file`, which a strip leaves as it is.
pub(crate) fn strip_lines(source: &str, file: &Path, lines: &mut [Value]) -> bool {
    let Some(drive) = drive_for(source, file) else {
        return false;
    };
    strip_with(source, &drive, file, lines);
    true
}

/// Each line's count of subtrees blanked, `lines` rewritten in place.
fn strip_with(source: &str, drive: &Drive, file: &Path, lines: &mut [Value]) -> Vec<usize> {
    let stamped = is_hook_envelope(file);
    let base = events_of(source, drive, lines, !stamped);
    (0..lines.len())
        .map(|i| probe(source, drive, lines, &base, i, "", stamped))
        .collect()
}

/// What a leaf [`blank_of`] could have blanked holds while the decoders are
/// asked whether they read it, a value of its own type; `None` for any other
/// leaf. A string's is path-shaped and under
/// [`SUBAGENTS_DIR`](crate::source::decoder::SUBAGENTS_DIR), so a decoder that
/// parses a nested transcript's parent out of one reads it.
fn sentinel_for(leaf: &Value) -> Option<Value> {
    match leaf {
        Value::String(s) if s.is_empty() => Some(Value::String(format!(
            "/pixtuoid-sentinel/{}/pixtuoid-sentinel.jsonl",
            crate::source::decoder::SUBAGENTS_DIR
        ))),
        Value::Number(n) if n.as_f64() == Some(0.0) => Some(Value::from(1)),
        Value::Bool(false) => Some(Value::Bool(true)),
        _ => None,
    }
}

/// Each blank leaf under `at` in `v`, by pointer, a [`kept_key`] aside.
fn blank_leaves(source: &str, stamped: bool, v: &Value, at: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => m
            .iter()
            .filter(|(k, _)| !kept_key(source, stamped, at, k))
            .for_each(|(k, x)| blank_leaves(source, stamped, x, &child_pointer(at, k), out)),
        Value::Array(a) => a
            .iter()
            .enumerate()
            .for_each(|(k, x)| blank_leaves(source, stamped, x, &format!("{at}/{k}"), out)),
        leaf if sentinel_for(leaf).is_some() => out.push(at.to_owned()),
        _ => {}
    }
}

/// Each blank leaf of `lines` that changes what the registry derives once it
/// holds its [`sentinel_for`], as `<line>:<pointer>`, the line counted from 1:
/// a field the decoders read that the bytes hold no value for. Whether the
/// strip blanked it or the wire sent it blank, the bytes cannot say, so a
/// capture's record keeps the set its strip saw, and one a decoder reads LATER
/// is a field that decoder never saw a value of
/// (`no_capture_blanked_a_field_a_decoder_now_reads`).
fn sentinel_reads_of(
    source: &str,
    drive: &Drive,
    lines: &mut [Value],
    stamped: bool,
) -> BTreeSet<String> {
    let base = events_of(source, drive, lines, !stamped);
    let mut read = BTreeSet::new();
    for i in 0..lines.len() {
        let mut blanks = Vec::new();
        blank_leaves(source, stamped, &lines[i], "", &mut blanks);
        for at in blanks {
            let Some(slot) = lines[i].pointer_mut(&at) else {
                continue;
            };
            let Some(sentinel) = sentinel_for(slot) else {
                continue;
            };
            let blank = std::mem::replace(slot, sentinel);
            if events_of(source, drive, lines, !stamped) != base {
                read.insert(format!("{}:{at}", i + 1));
            }
            if let Some(slot) = lines[i].pointer_mut(&at) {
                *slot = blank;
            }
        }
    }
    read
}

/// Write `bytes` over `file` through a sibling and a rename, so a reader never
/// sees a half-written capture. Std rather than `tempfile`, which
/// `deny-arch.toml`'s allowlist does not carry (architecture invariant #1). The
/// sibling keeps the original's permissions and is removed if any step fails.
fn replace_atomically(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = sibling_of(file);
    let written = (|| {
        let mut f = std::fs::File::create_new(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::set_permissions(&tmp, std::fs::metadata(file)?.permissions())?;
        std::fs::rename(&tmp, file)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// A name beside `file`, unique per process and call: two strips of one file
/// never share a sibling.
fn sibling_of(file: &Path) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = file.as_os_str().to_owned();
    name.push(format!(".{}-{n}.tmp", std::process::id()));
    PathBuf::from(name)
}

/// Strip every capture under `root` in place — local and unbilled — and
/// record the count in its provenance.
/// `blanked` counts every subtree blanked from the original recording: a pass
/// adds to the record's count, and one that blanks nothing leaves it alone.
/// `report` hears each capture as soon as its rewrite lands, so a later failure
/// cannot hide one that already happened.
///
/// # Errors
///
/// If a provenance or capture file cannot be read, parsed or rewritten.
pub fn strip_corpus(root: &Path, mut report: impl FnMut(&Capture, &Strip)) -> std::io::Result<()> {
    for capture in captures(root) {
        let path = capture.provenance_path();
        let mut record = Provenance::read(&path)?;
        let Provenance::Recorded(r) = &mut record;
        let files = capture.jsonl_files();
        if files.is_empty() {
            continue;
        }
        let strip = strip_unread(&capture.source, &files)?;
        let mut changed = false;
        if strip.blanked > 0 {
            let Deidentified::Stripped { blanked } = r.deidentified;
            r.deidentified = Deidentified::Stripped {
                blanked: blanked + strip.blanked,
            };
            changed = true;
        }
        // Shrink only: a read the record lacks is one a decoder grew after the
        // capture was stripped, which only a re-recording may add.
        for (file, now) in &strip.sentinel_reads {
            let key = super::slashed_under(&capture.dir, file)?;
            if let Some(kept) = r.sentinel_reads.get_mut(&key) {
                let before = kept.len();
                kept.retain(|read| now.contains(read));
                changed |= kept.len() != before;
            }
        }
        let named = files
            .iter()
            .map(|f| super::slashed_under(&capture.dir, f))
            .collect::<std::io::Result<BTreeSet<_>>>()?;
        let before = r.sentinel_reads.len();
        r.sentinel_reads
            .retain(|file, reads| !reads.is_empty() && named.contains(file));
        changed |= r.sentinel_reads.len() != before;
        if changed {
            record.write_to(&path)?;
        }
        report(&capture, &strip);
    }
    Ok(())
}

/// A hook envelope file, by the name the recorder gives it, `.new` included.
pub(crate) fn is_hook_envelope(file: &Path) -> bool {
    file.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(crate::harness::HOOK_PAYLOADS))
}

fn drive_for(source: &str, file: &Path) -> Option<Drive> {
    if is_hook_envelope(file) {
        Some(Drive::hooks())
    } else {
        Drive::transcript(source, &file.to_string_lossy())
    }
}

type Parsed = (Vec<String>, Vec<Value>);

fn parsed_lines(file: &Path) -> std::io::Result<Result<Parsed, serde_json::Error>> {
    let text = std::fs::read_to_string(file)?;
    let mut raw = Vec::new();
    let mut lines = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str(line) {
            Ok(v) => {
                raw.push(line.to_string());
                lines.push(v);
            }
            Err(e) => return Ok(Err(e)),
        }
    }
    Ok(Ok((raw, lines)))
}

type Decoded = (
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<Option<DecodedPresence>>,
    Vec<String>,
);

/// Everything the registry reads from a line, not only the line decoder: a
/// daemon's envelopes decode to no `AgentEvent` by design (presence rides
/// `presence_decoder`), and the watcher registers a `transcript` through
/// `cwd_extractor` and `parent_extractor`, which the harness never calls — it
/// seeds a fixed cwd and no parent. A hook envelope never meets them.
/// Compared as rendered, the way a golden is: `PathBuf` equality drops a
/// trailing slash, and grok's `workspaceRoot` fallback differs by exactly that.
fn events_of(source: &str, drive: &Drive, lines: &[Value], transcript: bool) -> Decoded {
    let d = drive.lines(lines.iter().map(Value::to_string));
    let parent = registry::parent_extractor_for(source);
    let first_sight = lines
        .iter()
        .filter(|_| transcript)
        .map(|l| {
            format!(
                "{:?} {:?}",
                registry::cwd_extractor_for(source)(l),
                parent.and_then(|f| f(l))
            )
        })
        .collect();
    let presence = lines
        .iter()
        .map(|line| {
            let obj = line.as_object()?;
            let src = obj.get(crate::source::decoder::SOURCE_STAMP)?.as_str()?;
            registry::presence_decoder_for(src)?(obj).ok()
        })
        .collect();
    let failures = |f: &[LineFailure]| f.iter().map(|x| format!("{x:?}")).collect::<Vec<_>>();
    (
        d.events.iter().map(|e| format!("{e:?}")).collect(),
        failures(&d.decode_errors),
        d.breadcrumbs,
        presence,
        first_sight,
    )
}

/// What blanking writes over `node`, its type's empty value; `None` when that
/// would leave it as it is (a null, or a value already empty).
pub(crate) fn blank_of(node: &Value) -> Option<Value> {
    let blank = match node {
        Value::Null => return None,
        Value::Bool(_) => Value::Bool(false),
        Value::Number(_) => Value::from(0),
        Value::String(_) => Value::String(String::new()),
        Value::Array(_) => Value::Array(Vec::new()),
        Value::Object(_) => Value::Object(serde_json::Map::new()),
    };
    (blank != *node).then_some(blank)
}

/// The JSON pointer to `key` under `at`, escaped (RFC 6901).
pub(crate) fn child_pointer(at: &str, key: &str) -> String {
    format!("{at}/{}", key.replace('~', "~0").replace('/', "~1"))
}

/// Whether `key` under `at` is a shim stamp, a hook envelope's top-level `_`
/// key, which the strip keeps: `_shim_ts_ms` is read by no decoder, only by
/// the merge (`merge::SHIM_STAMP`) and `tests/sources/captures.rs`.
pub(crate) fn is_shim_stamp(at: &str, key: &str) -> bool {
    at.is_empty() && key.starts_with('_')
}

/// How a transcript line spells the instant its CLI wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spelling {
    /// An RFC 3339 string.
    Rfc3339,
    /// An integer count of seconds since the epoch.
    EpochSecs,
}

/// Each source whose transcript lines carry their write stamp: the top-level
/// key it rides, and its spelling.
pub(crate) const WRITE_STAMPS: &[(&str, &str, Spelling)] = &[
    ("codex", "timestamp", Spelling::Rfc3339),
    ("grok", "timestamp", Spelling::EpochSecs),
];

/// Whether `key` under `at` is a `transcript`'s write stamp, which the strip
/// keeps though no decoder reads it: the merge test (`tests/sources/merge.rs`)
/// orders a session's two streams by it.
pub(crate) fn is_write_stamp(source: &str, transcript: bool, at: &str, key: &str) -> bool {
    transcript
        && at.is_empty()
        && WRITE_STAMPS
            .iter()
            .any(|&(s, k, _)| s == source && k == key)
}

/// Whether `key` under `at` is one the strip keeps whatever the decoders read:
/// a hook envelope's shim stamp, or a transcript's write stamp.
fn kept_key(source: &str, stamped: bool, at: &str, key: &str) -> bool {
    (stamped && is_shim_stamp(at, key)) || is_write_stamp(source, !stamped, at, key)
}

/// Each non-empty string leaf of `v` under `at`, as `(pointer, value)`, a
/// string that holds JSON read on under its own pointer. A shim stamp counts:
/// `_pixtuoid_event` is the only place a hook's name arrives for a CLI that
/// does not send it (antigravity).
pub(crate) fn string_leaves(v: &Value, at: &str, out: &mut BTreeSet<(String, String)>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                string_leaves(x, &child_pointer(at, k), out);
            }
        }
        Value::Array(a) => {
            for x in a {
                string_leaves(x, &format!("{at}/*"), out);
            }
        }
        Value::String(s) if !s.is_empty() => {
            if let Some(inner) = embedded_json(s) {
                string_leaves(&inner, at, out);
            }
            out.insert((at.to_owned(), s.clone()));
        }
        _ => {}
    }
}

/// The JSON a string holds, which the censuses read on into: codex's
/// `arguments` keys its call's fields.
pub(crate) fn embedded_json(s: &str) -> Option<Value> {
    s.starts_with(['{', '['])
        .then(|| serde_json::from_str(s).ok())
        .flatten()
}

fn probe(
    source: &str,
    drive: &Drive,
    lines: &mut [Value],
    base: &Decoded,
    i: usize,
    at: &str,
    stamped: bool,
) -> usize {
    let slot = lines[i]
        .pointer_mut(at)
        .expect("`at` is a pointer into the line's own value");
    let Some(neutral) = blank_of(slot) else {
        return 0;
    };
    let kept = std::mem::replace(slot, neutral);
    if events_of(source, drive, lines, !stamped) == *base {
        return 1;
    }
    let children: Vec<String> = match &kept {
        Value::Array(a) => (0..a.len()).map(|k| format!("{at}/{k}")).collect(),
        Value::Object(m) => m
            .keys()
            .filter(|k| !kept_key(source, stamped, at, k))
            .map(|k| child_pointer(at, k))
            .collect(),
        _ => Vec::new(),
    };
    *lines[i]
        .pointer_mut(at)
        .expect("`at` is a pointer into the line's own value") = kept;
    children
        .iter()
        .map(|child| probe(source, drive, lines, base, i, child, stamped))
        .sum()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::harness::sources_root;

    #[test]
    fn a_child_pointer_escapes_tilde_before_slash() {
        assert_eq!(child_pointer("/a", "b"), "/a/b");
        assert_eq!(child_pointer("", "x~y/z"), "/x~0y~1z");
        assert_eq!(
            child_pointer("", "~1"),
            "/~01",
            "a literal ~1 stays a tilde and a 1"
        );
    }

    fn scenario_events(source: &str, files: &[PathBuf]) -> Vec<Decoded> {
        files
            .iter()
            .filter_map(|f| {
                let (_, lines) = parsed_lines(f).ok()?.ok()?;
                Some(events_of(
                    source,
                    &drive_for(source, f)?,
                    &lines,
                    !is_hook_envelope(f),
                ))
            })
            .collect()
    }

    fn first_key(line: &str) -> &str {
        line.trim_start_matches('{')
            .split('"')
            .nth(1)
            .expect("an object with a key")
    }

    /// Plant a key no decoder reads on the first non-empty line, so the test
    /// holds whether or not the committed corpus has already been stripped.
    fn plant_unread(file: &Path, key: &str, value: &str) {
        let text = std::fs::read_to_string(file).expect("read");
        let mut planted = false;
        let out: Vec<String> = text
            .lines()
            .map(|l| {
                let mut v: serde_json::Value = serde_json::from_str(l).expect("json");
                if !planted && v.as_object().is_some_and(|o| !o.is_empty()) {
                    v[key] = serde_json::Value::String(value.into());
                    planted = true;
                }
                serde_json::to_string(&v).expect("json")
            })
            .collect();
        assert!(planted, "no non-empty line to plant on");
        std::fs::write(file, out.join("\n") + "\n").expect("write");
    }

    fn transcript_in(source: &str, scenario: &Path) -> PathBuf {
        crate::harness::transcript_in(source, scenario).unwrap_or_else(|found| {
            panic!(
                "{}: one transcript per scenario, found {found:?}",
                scenario.display()
            )
        })
    }

    fn stripped_copy_of(src: &Path) -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().expect("tempdir");
        let to = d.path().join(src.file_name().expect("file name"));
        std::fs::copy(src, &to).expect("copy");
        (d, to)
    }

    fn stripped_copy(source: &str, scenario: &str, name: &str) -> (tempfile::TempDir, PathBuf) {
        stripped_copy_of(
            &crate::harness::fixtures_dir(&crate::harness::sources_root(), source)
                .join(scenario)
                .join(name),
        )
    }

    #[test]
    fn a_daemon_capture_keeps_what_its_presence_decoder_reads() {
        let (_d, hooks) = stripped_copy("openclaw", "tool-run-hermetic", "hook-payloads.jsonl");
        let files = vec![hooks.clone()];
        let before = scenario_events("openclaw", &files);
        let wire = std::fs::read_to_string(&hooks).expect("read");
        strip_unread("openclaw", &files).expect("strip");
        assert_eq!(scenario_events("openclaw", &files), before);
        let stripped = std::fs::read_to_string(&hooks).expect("read");
        assert_eq!(
            stripped.lines().next(),
            wire.lines().next(),
            "every field of `gateway_start` is read or a shim stamp, so the line is untouched"
        );
    }

    /// `PathBuf` equality drops the trailing slash a rendered event keeps, so a
    /// probe comparing paths would blank `cwd` here, its fallback naming the
    /// same directory with a slash. No current CLI's wire has the shape (grok's
    /// before 1.0.70 did), so the line is written here.
    #[test]
    fn a_cwd_whose_fallback_differs_by_a_trailing_slash_survives_the_strip() {
        let d = tempfile::tempdir().expect("tempdir");
        let hooks = d.path().join(crate::harness::HOOK_PAYLOADS);
        std::fs::write(
            &hooks,
            r#"{"hookEventName":"session_start","sessionId":"s1","cwd":"/private/tmp/pixtuoid-capture/proj","workspaceRoot":"/private/tmp/pixtuoid-capture/proj/","_pixtuoid_source":"grok"}"#,
        )
        .expect("write");
        let rendered = |p: &Path| -> Vec<String> {
            Drive::hooks()
                .lines(std::fs::read_to_string(p).expect("read").lines())
                .events
                .iter()
                .map(|e| format!("{e:?}"))
                .collect()
        };
        let before = rendered(&hooks);
        assert!(before.iter().any(|e| e.contains("pixtuoid-capture/proj\"")));
        strip_unread("grok", std::slice::from_ref(&hooks)).expect("strip");
        assert_eq!(rendered(&hooks), before);
    }

    #[test]
    fn the_first_sight_cwd_survives_the_strip() {
        let (_d, rollout) = stripped_copy_of(&transcript_in(
            "codex",
            &sources_root().join("codex/fixtures/tool-run-hermetic"),
        ));
        let cwds = |p: &Path| -> Vec<Option<PathBuf>> {
            let extract = registry::cwd_extractor_for("codex");
            parsed_lines(p)
                .expect("read")
                .expect("json")
                .1
                .iter()
                .map(extract)
                .collect()
        };
        let before = cwds(&rollout);
        assert!(
            before
                .iter()
                .any(|c| c.as_ref().is_some_and(|p| !p.as_os_str().is_empty())),
            "the rollout carries the cwd first-sight registers"
        );
        strip_unread("codex", std::slice::from_ref(&rollout)).expect("strip");
        assert_eq!(cwds(&rollout), before);
    }

    /// First-sight registers a transcript, never a hook envelope, so a hook
    /// line's cwd survives only where a decoder reads it: hermes's finalize,
    /// outside any turn, reads none, and its every in-turn event reads its own.
    #[test]
    fn a_hook_lines_cwd_is_unread_unless_a_decoder_reads_it() {
        const CWD: &str = "/planted/proj";
        let (_d, hooks) = stripped_copy("hermes", "permission-hermetic", "hook-payloads.jsonl");
        let (_, mut lines) = parsed_lines(&hooks).expect("read").expect("json");
        for line in &mut lines {
            line["cwd"] = CWD.into();
        }
        let text: Vec<String> = lines.iter().map(Value::to_string).collect();
        std::fs::write(&hooks, text.join("\n") + "\n").expect("write");

        strip_unread("hermes", std::slice::from_ref(&hooks)).expect("strip");

        let (_, stripped) = parsed_lines(&hooks).expect("read").expect("json");
        let outside = crate::source::hermes::OUTSIDE_A_TURN;
        for line in &stripped {
            let event = line["hook_event_name"].as_str().expect("an event");
            let want = if outside.contains(&event) { "" } else { CWD };
            assert_eq!(line["cwd"], want, "{event}");
        }
        assert!(
            stripped
                .iter()
                .any(|l| outside.contains(&l["hook_event_name"].as_str().unwrap_or_default())),
            "the capture holds an event outside a turn"
        );
    }

    #[test]
    fn the_shim_stamps_survive_the_strip() {
        let (_d, hooks) = stripped_copy("claude-code", "tool-run-hermetic", "hook-payloads.jsonl");
        let stamps = |p: &Path| -> Vec<serde_json::Value> {
            parsed_lines(p)
                .expect("read")
                .expect("json")
                .1
                .iter()
                .map(|l| l["_shim_ts_ms"].clone())
                .collect()
        };
        let before = stamps(&hooks);
        assert!(before.iter().all(|v| v.as_i64().is_some_and(|ms| ms > 0)));
        strip_unread("claude-code", std::slice::from_ref(&hooks)).expect("strip");
        assert_eq!(stamps(&hooks), before);
    }

    #[test]
    fn a_transcripts_write_stamp_survives_the_strip_where_its_line_does() {
        const STAMP: &str = "2026-10-27T04:47:02.517Z";
        let (_d, rollout) = stripped_copy_of(&transcript_in(
            "codex",
            &sources_root().join("codex/fixtures/tool-run-hermetic"),
        ));
        let (_e, hooks) = stripped_copy("codex", "tool-run-hermetic", "hook-payloads.jsonl");
        for file in [&rollout, &hooks] {
            let (_, mut lines) = parsed_lines(file).expect("read").expect("json");
            for line in &mut lines {
                if let Some(o) = line.as_object_mut().filter(|o| !o.is_empty()) {
                    o.insert("timestamp".into(), STAMP.into());
                }
            }
            let text: Vec<String> = lines.iter().map(Value::to_string).collect();
            std::fs::write(file, text.join("\n") + "\n").expect("write");
        }
        strip_unread("codex", &[rollout.clone(), hooks.clone()]).expect("strip");
        let (_, rollout) = parsed_lines(&rollout).expect("read").expect("json");
        let kept: Vec<&Value> = rollout
            .iter()
            .filter(|l| l.as_object().is_some_and(|o| !o.is_empty()))
            .collect();
        assert!(!kept.is_empty(), "a read line survives");
        for line in kept {
            assert_eq!(line["timestamp"], STAMP, "{line}");
        }
        let (_, hooks) = parsed_lines(&hooks).expect("read").expect("json");
        for line in &hooks {
            assert_eq!(
                line["timestamp"], "",
                "a hook's own stamp is no transcript's write stamp: {line}"
            );
        }
    }

    #[test]
    fn stripping_blanks_what_no_decoder_reads_and_changes_no_event() {
        let d = tempfile::tempdir().expect("tempdir");
        let src = sources_root().join("codex/fixtures/tool-run-hermetic");
        let files: Vec<PathBuf> = [
            transcript_in("codex", &src),
            src.join("hook-payloads.jsonl"),
        ]
        .iter()
        .map(|from| {
            let to = d.path().join(from.file_name().expect("file name"));
            std::fs::copy(from, &to).expect("copy");
            to
        })
        .collect();

        plant_unread(&files[0], "installed_skills", "example-skill-1");
        let before = scenario_events("codex", &files);
        let wire = std::fs::read_to_string(&files[0]).expect("read");
        let model = wire
            .lines()
            .find_map(|l| {
                let v: Value = serde_json::from_str(l).ok()?;
                v["payload"]["model"].as_str().map(str::to_owned)
            })
            .filter(|m| !m.is_empty())
            .expect("a turn's model");
        let session_id = first_hook_field(&files[1], "session_id");
        let blanked = strip_unread("codex", &files).expect("strip").blanked;
        assert!(blanked > 0, "the planted key is unread");
        assert_eq!(scenario_events("codex", &files), before);

        let transcript = std::fs::read_to_string(&files[0]).expect("read");
        let rewritten = wire
            .lines()
            .zip(transcript.lines())
            .find(|(w, t)| w != t && *t != "{}")
            .expect("a line rewritten but not collapsed");
        assert_eq!(
            first_key(rewritten.1),
            first_key(rewritten.0),
            "a rewritten line keeps the wire's key order"
        );
        assert!(
            !transcript.contains("example-skill"),
            "a roster rides a field no decoder reads, so it leaves"
        );
        assert!(
            transcript.contains(&model),
            "the model is read, so it stays"
        );
        let hooks = std::fs::read_to_string(&files[1]).expect("read");
        assert!(
            hooks.contains(&session_id),
            "the hook's session_id keys coalescing, so it stays"
        );
    }

    /// A string field off the first hook payload, so an assertion follows the
    /// capture rather than pinning one recording's id. Refuses a blank: read
    /// after the strip, `contains("")` would pass on exactly the wrongly
    /// stripped shape.
    fn first_hook_field(hooks: &Path, key: &str) -> String {
        let line = std::fs::read_to_string(hooks).expect("read");
        let line = line.lines().next().expect("a hook line");
        let value = serde_json::from_str::<serde_json::Value>(line).expect("json")[key]
            .as_str()
            .unwrap_or_else(|| panic!("{key} on the first hook payload"))
            .to_string();
        assert!(
            !value.is_empty(),
            "{key} on the first hook payload is blank"
        );
        value
    }

    /// [`sentinel_reads_of`] over `file` as it stands; `None` where no decoder
    /// reads it or a line is not JSON.
    ///
    /// # Errors
    ///
    /// If `file` cannot be read.
    fn sentinel_reads(source: &str, file: &Path) -> std::io::Result<Option<BTreeSet<String>>> {
        let (Some(drive), Ok((_, mut lines))) = (drive_for(source, file), parsed_lines(file)?)
        else {
            return Ok(None);
        };
        Ok(Some(sentinel_reads_of(
            source,
            &drive,
            &mut lines,
            is_hook_envelope(file),
        )))
    }

    /// Each read of a capture under `root` that its record lacks, as
    /// `<file> <line>:<pointer>`.
    fn unrecorded_reads(root: &Path) -> Vec<String> {
        let mut stale = Vec::new();
        for capture in captures(root) {
            let record = Provenance::read(&capture.provenance_path()).expect("a record parses");
            let kept = &record.recorded().sentinel_reads;
            for file in capture.jsonl_files() {
                let Some(now) = sentinel_reads(&capture.source, &file).expect("read") else {
                    continue;
                };
                let then =
                    kept.get(&crate::harness::slashed_under(&capture.dir, &file).expect("under"));
                for read in now.iter().filter(|r| !then.is_some_and(|t| t.contains(*r))) {
                    let at = crate::harness::slashed_under(root, &file).expect("under");
                    stale.push(format!("{at} {read}"));
                }
            }
        }
        stale
    }

    /// A field a capture's strip blanked, once a decoder reads it, holds no
    /// value that decoder can see: the golden encodes "absent" where the wire
    /// carried one, so the scenario is owed a re-recording. Judged against the
    /// reads its record kept when the strip ran.
    #[test]
    fn no_capture_blanked_a_field_a_decoder_now_reads() {
        let stale = unrecorded_reads(&sources_root());
        assert!(
            stale.is_empty(),
            "a decoder reads what these captures' strip blanked; re-record each scenario:\n  {}",
            stale.join("\n  ")
        );
    }

    #[test]
    fn stripping_a_committed_scenario_keeps_its_events_and_a_recordings_bytes() {
        let root = sources_root();
        let mut walked = BTreeSet::new();
        for capture in captures(&root) {
            let (scenario, source) = (&capture.dir, &capture.source);
            let label = scenario
                .strip_prefix(&root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            let jsonl = capture.jsonl_files();
            assert!(
                !jsonl.is_empty() || !scenario.join("provenance.json").is_file(),
                "{label}: a provenance with no bytes under it"
            );
            if jsonl.is_empty() {
                continue;
            }
            let d = tempfile::tempdir().expect("tempdir");
            let files: Vec<PathBuf> = jsonl
                .iter()
                .map(|from| {
                    let to = d.path().join(from.strip_prefix(scenario).expect("under"));
                    std::fs::create_dir_all(to.parent().expect("parent")).expect("mkdir");
                    std::fs::copy(from, &to).expect("copy");
                    to
                })
                .collect();
            assert!(
                registry::descriptor_for(source).is_some(),
                "{label}: {source:?} is not a registered source, so every rule keyed on it misses"
            );
            for f in &files {
                assert!(
                    drive_for(source, f).is_some(),
                    "{label}: {} cannot be routed as {source}, so the walk would skip it",
                    f.display()
                );
            }
            let before = scenario_events(source, &files);
            let strip = strip_unread(source, &files).expect("strip");
            assert_eq!(scenario_events(source, &files), before, "{label}");
            let recorded = Provenance::read_if_any(&capture.provenance_path())
                .expect("a committed record parses")
                .is_some();
            assert!(
                !recorded || strip.blanked == 0,
                "{label}: {} subtrees no decoder reads any more; `just restrip-fixtures`",
                strip.blanked
            );
            walked.insert(label);
        }
        for sentinel in [
            "codex/fixtures/tool-run-hermetic",
            "claude_code/fixtures/tool-run-hermetic",
            "omp/fixtures/tool-run-hermetic",
            "cursor/fixtures/delegation",
            "copilot/fixtures/subagent-hermetic",
            "copilot/fixtures/tool-run-hermetic",
            "grok/fixtures/tool-run-hermetic",
        ] {
            assert!(
                walked.contains(sentinel),
                "walk missed {sentinel}: {walked:?}"
            );
        }
    }

    /// `capture`, a dir under the corpus, copied to the same path under `root`.
    fn corpus_copy(root: &Path, capture: &str) -> PathBuf {
        let (from, to) = (sources_root().join(capture), root.join(capture));
        for file in crate::harness::files_under(&from) {
            let at = to.join(file.strip_prefix(&from).expect("under"));
            std::fs::create_dir_all(at.parent().expect("a parent")).expect("mkdir");
            std::fs::copy(&file, at).expect("copy");
        }
        to
    }

    /// Each file of `dir` with its bytes, in path order.
    fn bytes_of(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out: Vec<_> = std::fs::read_dir(dir)
            .expect("read")
            .flatten()
            .map(|e| (e.path(), std::fs::read(e.path()).expect("bytes")))
            .collect();
        out.sort();
        out
    }

    /// A restrip keeps a read the decoders stopped making out of the record, and
    /// never writes one in: a read the record lacks is the detector's to name.
    #[test]
    fn a_restrip_only_shrinks_a_records_sentinel_reads() {
        let d = tempfile::tempdir().expect("tempdir");
        let scenario = corpus_copy(d.path(), "grok/fixtures/tool-run-hermetic");
        let prov = scenario.join("provenance.json");
        let reads = || {
            Provenance::read(&prov)
                .expect("parses")
                .recorded()
                .sentinel_reads
                .clone()
        };
        let real = reads();
        assert!(!real.is_empty(), "grok's hooks carry a read blank `cwd`");
        strip_corpus(d.path(), |_, _| {}).expect("strip corpus");
        assert_eq!(
            reads(),
            real,
            "a restrip keeps every read the decoders still make"
        );
        let mut record = Provenance::read(&prov).expect("parses");
        let Provenance::Recorded(r) = &mut record;
        for kept in r.sentinel_reads.values_mut() {
            kept.insert("1:/no-longer-read".into());
        }
        r.sentinel_reads
            .insert("gone.jsonl".into(), ["1:/x".into()].into());
        record.write_to(&prov).expect("write");
        strip_corpus(d.path(), |_, _| {}).expect("strip corpus");
        assert_eq!(reads(), real, "the bogus reads go, the real ones stay");

        let mut record = Provenance::read(&prov).expect("parses");
        let Provenance::Recorded(r) = &mut record;
        r.sentinel_reads.clear();
        record.write_to(&prov).expect("write");
        strip_corpus(d.path(), |_, _| {}).expect("strip corpus");
        assert!(reads().is_empty(), "a restrip never adds a read");
    }

    /// The detector over a copy of a committed capture whose record lost one
    /// of its reads: it names exactly that one, so an empty answer over the
    /// corpus is a verdict, not a probe that found nothing.
    #[test]
    fn the_detector_names_exactly_a_read_its_record_lacks() {
        let d = tempfile::tempdir().expect("tempdir");
        let capture = "grok/fixtures/tool-run-hermetic";
        let scenario = corpus_copy(d.path(), capture);
        assert_eq!(unrecorded_reads(d.path()), Vec::<String>::new());
        let prov = scenario.join("provenance.json");
        let mut record = Provenance::read(&prov).expect("parses");
        let Provenance::Recorded(r) = &mut record;
        let (file, kept) = r.sentinel_reads.iter_mut().next().expect("a recorded read");
        let dropped = kept.pop_first().expect("a read");
        let named = format!("{capture}/{file} {dropped}");
        record.write_to(&prov).expect("write");
        assert_eq!(unrecorded_reads(d.path()), [named]);
    }

    /// Every blank [`blank_of`] writes is probed with a value of its own type,
    /// and only a blank one is: a container the strip collapsed has no keys
    /// left to stand in for.
    #[test]
    fn every_blank_leaf_is_probed_with_a_value_of_its_type() {
        use serde_json::json;
        for leaf in [json!(""), json!(0), json!(0.0), json!(false)] {
            let sentinel = sentinel_for(&leaf).expect("probed");
            assert_eq!(
                std::mem::discriminant(&sentinel),
                std::mem::discriminant(&leaf),
                "{leaf}"
            );
            assert!(
                blank_of(&sentinel).is_some(),
                "{leaf}: {sentinel} is not blank"
            );
        }
        for leaf in [
            json!("x"),
            json!(3),
            json!(true),
            json!(null),
            json!({}),
            json!([]),
        ] {
            assert_eq!(sentinel_for(&leaf), None, "{leaf}");
        }
    }

    #[test]
    fn strip_corpus_records_its_count_in_the_provenance_and_is_idempotent() {
        let d = tempfile::tempdir().expect("tempdir");
        let scenario = corpus_copy(d.path(), "codex/fixtures/tool-run-hermetic");
        plant_unread(
            &transcript_in("codex", &scenario),
            "installed_skills",
            "example-skill-1",
        );
        let wire = bytes_of(&scenario);
        let record = || -> serde_json::Value {
            serde_json::from_str(
                &std::fs::read_to_string(scenario.join("provenance.json")).expect("read"),
            )
            .expect("json")
        };
        let recorded = record()["deidentified"]["blanked"]
            .as_u64()
            .expect("a count");

        strip_corpus(d.path(), |_, _| {}).expect("strip corpus");
        let once = bytes_of(&scenario);
        assert_ne!(once, wire, "the planted key is unread");
        let prov = record();
        assert_eq!(prov["deidentified"]["method"], "decoder-allowlist");
        assert_eq!(
            prov["deidentified"]["blanked"].as_u64(),
            Some(recorded + 1),
            "the planted key adds to the recording's count"
        );
        assert_eq!(prov["origin"], "recorded", "the rest of the record is kept");

        strip_corpus(d.path(), |_, _| {}).expect("strip corpus again");
        assert_eq!(
            bytes_of(&scenario),
            once,
            "a second pass finds nothing and rewrites nothing"
        );
    }

    #[test]
    fn a_file_with_a_non_json_line_is_left_whole_and_named_with_its_error() {
        let d = tempfile::tempdir().expect("tempdir");
        let file = d.path().join("hook-payloads.jsonl");
        let wire = "{\"hook_event_name\":\"SessionStart\",\"stray\":1}\nnot json\n";
        std::fs::write(&file, wire).expect("write");
        let strip = strip_unread("claude-code", std::slice::from_ref(&file)).expect("strip");
        assert_eq!(strip.blanked, 0, "nothing in a skipped file is blanked");
        assert_eq!(
            std::fs::read_to_string(&file).expect("read"),
            wire,
            "left as recorded"
        );
        let [(named, error)] = strip.not_json.as_slice() else {
            panic!("one file named, got {:?}", strip.not_json);
        };
        assert_eq!(named, &file);
        assert!(!error.is_empty(), "the parse error rides along");
    }

    #[test]
    fn replace_atomically_replaces_the_file_and_leaves_no_sibling() {
        let d = tempfile::tempdir().expect("tempdir");
        let file = d.path().join("payloads.jsonl");
        std::fs::write(&file, "old\n").expect("write");
        replace_atomically(&file, b"new\n").expect("replace");
        assert_eq!(std::fs::read_to_string(&file).expect("read"), "new\n");
        let left: Vec<_> = std::fs::read_dir(d.path())
            .expect("read")
            .flatten()
            .collect();
        assert_eq!(left.len(), 1, "no sibling survives: {left:?}");
    }

    #[cfg(unix)]
    #[test]
    fn replace_atomically_keeps_the_originals_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().expect("tempdir");
        let file = d.path().join("payloads.jsonl");
        std::fs::write(&file, "old\n").expect("write");
        // An execute bit: no umask gives a new file one, so a rewrite that
        // dropped the copy fails whatever the umask.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        replace_atomically(&file, b"new\n").expect("replace");
        let mode = std::fs::metadata(&file).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "the capture keeps its mode");
    }

    #[test]
    fn replace_atomically_removes_its_sibling_when_the_rename_fails() {
        let d = tempfile::tempdir().expect("tempdir");
        // A directory where the file should be: the rename onto it fails.
        let file = d.path().join("payloads.jsonl");
        std::fs::create_dir(&file).expect("mkdir");
        std::fs::write(file.join("inside"), "x").expect("write");
        assert!(replace_atomically(&file, b"new\n").is_err());
        let left: Vec<String> = std::fs::read_dir(d.path())
            .expect("read")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left,
            vec!["payloads.jsonl".to_string()],
            "the sibling is gone"
        );
    }

    #[test]
    fn strip_corpus_reports_each_capture_before_a_later_failure() {
        let d = tempfile::tempdir().expect("tempdir");
        let good = corpus_copy(d.path(), "codex/fixtures/tool-run-hermetic");
        // Walked after codex (the walk sorts), and its record does not parse.
        let broken = d.path().join("opencode/fixtures/zz-broken");
        std::fs::create_dir_all(&broken).expect("mkdir");
        std::fs::write(broken.join("provenance.json"), "not json").expect("write");
        let mut heard = Vec::new();
        let result = strip_corpus(d.path(), |c, _| heard.push(c.dir.clone()));
        assert!(result.is_err(), "the broken record fails the pass");
        assert_eq!(
            heard,
            vec![good],
            "the capture already rewritten was reported"
        );
    }

    #[test]
    fn two_siblings_of_one_file_differ_and_name_the_process() {
        let file = Path::new("/x/payloads.jsonl");
        let (a, b) = (sibling_of(file), sibling_of(file));
        assert_ne!(a, b, "each call gets its own sibling");
        for s in [&a, &b] {
            assert_eq!(
                s.parent(),
                file.parent(),
                "beside the file, for an atomic rename"
            );
            assert!(
                s.to_string_lossy()
                    .contains(&format!(".{}-", std::process::id())),
                "{} names this process",
                s.display()
            );
        }
    }
}
