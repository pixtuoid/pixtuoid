//! What a decoder READS of a capture, derived by probe and never listed: a
//! subtree is unread when neutralizing it leaves everything the registry derives
//! from the line unchanged. The recorder strips captures with it, so a field a
//! decoder stopped reading cannot linger in committed bytes.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::provenance::{Deidentified, Provenance};
use super::{Capture, Drive, LineFailure, captures};
use crate::source::daemon::DecodedPresence;
use crate::source::registry;

/// One strip pass: how many subtrees were blanked, and which files were left as
/// recorded because a line in them is not JSON.
#[derive(Debug, Default)]
pub struct Strip {
    /// Subtrees blanked across every file.
    pub blanked: usize,
    /// Files left whole, each with its first parse error: half-rewriting one
    /// would corrupt the bytes. The committed-tree gate refuses to skip such a
    /// line at all (`no_identity_key_holds_a_value_outside_a_pinned_exemption`).
    pub not_json: Vec<(PathBuf, String)>,
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
    let base = events_of(source, drive, lines);
    let stamped = is_hook_envelope(file);
    (0..lines.len())
        .map(|i| probe(source, drive, lines, &base, i, "", stamped))
        .collect()
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

/// Strip every RECORDED capture under `root` in place — local and unbilled — and
/// record the count in its provenance. A composed capture carries no operator
/// data by construction, and the README demo reads its bytes past the decoders.
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
        let Provenance::Recorded(r) = &mut record else {
            continue;
        };
        if !r.strips() {
            continue;
        }
        let files = capture.jsonl_files();
        if files.is_empty() {
            continue;
        }
        let strip = strip_unread(&capture.source, &files)?;
        if strip.blanked > 0 || r.deidentified.is_none() {
            let earlier = match r.deidentified {
                Some(Deidentified::Stripped { blanked }) => blanked,
                _ => 0,
            };
            r.deidentified = Some(Deidentified::Stripped {
                blanked: earlier + strip.blanked,
            });
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
    Vec<String>,
    Vec<Option<DecodedPresence>>,
    Vec<String>,
);

/// Everything the registry reads from a line, not only the line decoder: a
/// daemon's envelopes decode to no `AgentEvent` by design (presence rides
/// `presence_decoder`), and the watcher registers a transcript through
/// `cwd_extractor` and `parent_extractor`, which the harness never calls — it
/// seeds a fixed cwd and no parent.
/// Compared as rendered, the way a golden is: `PathBuf` equality drops a
/// trailing slash, and grok's `workspaceRoot` fallback differs by exactly that.
fn events_of(source: &str, drive: &Drive, lines: &[Value]) -> Decoded {
    let d = drive.lines(lines.iter().map(Value::to_string));
    let parent = registry::parent_extractor_for(source);
    let first_sight = lines
        .iter()
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
            let src = obj.get("_pixtuoid_source")?.as_str()?;
            registry::presence_decoder_for(src)?(obj).ok()
        })
        .collect();
    let failures = |f: &[LineFailure]| f.iter().map(|x| format!("{x:?}")).collect::<Vec<_>>();
    (
        d.events.iter().map(|e| format!("{e:?}")).collect(),
        failures(&d.decode_errors),
        failures(&d.panics),
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
/// `tests/sources/captures.rs`, which dates a capture by it.
pub(crate) fn is_shim_stamp(at: &str, key: &str) -> bool {
    at.is_empty() && key.starts_with('_')
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
    if events_of(source, drive, lines) == *base {
        return 1;
    }
    let children: Vec<String> = match &kept {
        Value::Array(a) => (0..a.len()).map(|k| format!("{at}/{k}")).collect(),
        Value::Object(m) => m
            .keys()
            .filter(|k| !(stamped && is_shim_stamp(at, k)))
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
                Some(events_of(source, &drive_for(source, f)?, &lines))
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
            &sources_root()
                .join("fixtures")
                .join(source)
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

    #[test]
    fn a_cwd_whose_fallback_differs_by_a_trailing_slash_survives_the_strip() {
        let (_d, hooks) = stripped_copy("grok", "gate-both-ways", "hook-payloads.jsonl");
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
            &sources_root().join("fixtures/codex/tool-run-recorded"),
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

    #[test]
    fn the_shim_stamps_survive_the_strip() {
        let (_d, hooks) = stripped_copy("claude-code", "tool-run-recorded", "hook-payloads.jsonl");
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
    fn stripping_blanks_what_no_decoder_reads_and_changes_no_event() {
        let d = tempfile::tempdir().expect("tempdir");
        let src = sources_root().join("fixtures/codex/tool-run-recorded");
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
            transcript.contains("gpt-5.6-sol"),
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
                .is_some_and(|record| record.strips());
            assert!(
                !recorded || strip.blanked == 0,
                "{label}: {} subtrees no decoder reads any more; `just restrip-fixtures`",
                strip.blanked
            );
            walked.insert(label);
        }
        for sentinel in [
            "fixtures/codex/tool-run-recorded",
            "fixtures/claude-code/tool-run-recorded",
            "omp/fixtures/bridge-run-recorded",
            "cursor/fixtures",
            "delegation/fixtures/copilot",
            "fixtures/copilot/tool-run-recorded",
            "fixtures/grok/tool-run-recorded",
        ] {
            assert!(
                walked.contains(sentinel),
                "walk missed {sentinel}: {walked:?}"
            );
        }
    }

    #[test]
    fn strip_corpus_leaves_a_composed_capture_alone() {
        let d = tempfile::tempdir().expect("tempdir");
        let scenario = d.path().join("fixtures/claude-code/proof-session");
        std::fs::create_dir_all(&scenario).expect("mkdir");
        let src = sources_root().join("fixtures/claude-code/proof-session");
        for from in std::fs::read_dir(&src).expect("read").flatten() {
            std::fs::copy(from.path(), scenario.join(from.file_name())).expect("copy");
        }
        let bytes = |dir: &Path| -> Vec<(PathBuf, Vec<u8>)> {
            let mut out: Vec<_> = std::fs::read_dir(dir)
                .expect("read")
                .flatten()
                .map(|e| (e.path(), std::fs::read(e.path()).expect("bytes")))
                .collect();
            out.sort();
            out
        };
        let before = bytes(&scenario);
        strip_corpus(d.path(), |_, _| {}).expect("strip corpus");
        assert_eq!(
            bytes(&scenario),
            before,
            "composed bytes carry no operator data"
        );
    }

    #[test]
    fn strip_corpus_honors_an_exemption_the_provenance_declares() {
        let d = tempfile::tempdir().expect("tempdir");
        let scenario = d.path().join("fixtures/codex/tool-run-recorded");
        std::fs::create_dir_all(&scenario).expect("mkdir");
        let src = sources_root().join("fixtures/codex/tool-run-recorded");
        for from in std::fs::read_dir(&src).expect("read").flatten() {
            std::fs::copy(from.path(), scenario.join(from.file_name())).expect("copy");
        }
        let prov = scenario.join("provenance.json");
        let mut record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&prov).expect("read")).expect("json");
        record["deidentified"] = serde_json::json!({
            "method": "none",
            "why": "pins a wire premise the decoder drops on purpose"
        });
        std::fs::write(&prov, serde_json::to_string_pretty(&record).expect("json")).expect("write");
        let bytes = |dir: &Path| -> Vec<(PathBuf, Vec<u8>)> {
            let mut out: Vec<_> = std::fs::read_dir(dir)
                .expect("read")
                .flatten()
                .map(|e| (e.path(), std::fs::read(e.path()).expect("bytes")))
                .collect();
            out.sort();
            out
        };
        let before = bytes(&scenario);
        strip_corpus(d.path(), |_, _| {}).expect("strip corpus");
        assert_eq!(
            bytes(&scenario),
            before,
            "an exempt capture keeps its bytes and its record"
        );
    }

    #[test]
    fn strip_corpus_records_its_count_in_the_provenance_and_is_idempotent() {
        let d = tempfile::tempdir().expect("tempdir");
        let scenario = d.path().join("fixtures/codex/tool-run-recorded");
        std::fs::create_dir_all(&scenario).expect("mkdir");
        let src = sources_root().join("fixtures/codex/tool-run-recorded");
        for from in std::fs::read_dir(&src).expect("read").flatten() {
            std::fs::copy(from.path(), scenario.join(from.file_name())).expect("copy");
        }
        let snapshot = |dir: &Path| -> Vec<(PathBuf, Vec<u8>)> {
            let mut out: Vec<_> = std::fs::read_dir(dir)
                .expect("read")
                .flatten()
                .map(|e| (e.path(), std::fs::read(e.path()).expect("bytes")))
                .collect();
            out.sort();
            out
        };
        plant_unread(
            &transcript_in("codex", &scenario),
            "installed_skills",
            "example-skill-1",
        );
        let wire = snapshot(&scenario);
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
        let once = snapshot(&scenario);
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
            snapshot(&scenario),
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
        let good = d.path().join("fixtures/codex/tool-run-recorded");
        std::fs::create_dir_all(&good).expect("mkdir");
        let src = sources_root().join("fixtures/codex/tool-run-recorded");
        for from in std::fs::read_dir(&src).expect("read").flatten() {
            std::fs::copy(from.path(), good.join(from.file_name())).expect("copy");
        }
        // Walked after codex (the walk sorts), and its record does not parse.
        let broken = d.path().join("fixtures/opencode/zz-broken");
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
