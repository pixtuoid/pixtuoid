//! The rules every committed capture obeys, over THE enumeration —
//! `pixtuoid_core::harness::captures`, shared with the recorder's re-strip.
//! Several walks of this tree with different populations used to exist, so each
//! rule landed on whichever subset its author picked — the "fix landed on half
//! the population" class that recurred across four review rounds. So: ONE walk,
//! and `the_walk_sees_every_provenance_on_disk` fails if a capture falls outside.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pixtuoid_core::source::registry;

/// A committed capture: the bytes, the record of where they came from, and the
/// source the LAYOUT says owns them.
pub(crate) struct Capture {
    pub(crate) dir: PathBuf,
    /// Resolved from the layout, never from the provenance — a record that lies
    /// about its `cli` must not get to pick which rules apply to it.
    pub(crate) source: String,
    pub(crate) provenance_path: PathBuf,
    pub(crate) provenance: serde_json::Value,
}

impl Capture {
    pub(crate) fn origin(&self) -> &str {
        self.provenance
            .get("origin")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    }

    pub(crate) fn is_recorded(&self) -> bool {
        self.origin() == "recorded"
    }

    pub(crate) fn field(&self, key: &str) -> Option<&str> {
        self.provenance.get(key).and_then(serde_json::Value::as_str)
    }

    /// The scenario's transcripts — every `.jsonl` that is not the hook payload
    /// file, RECURSIVELY: a parent-dir-keyed source nests its transcript one
    /// level down, and a flat read was blind to exactly the fixtures the
    /// nesting rule forces into that shape.
    pub(crate) fn transcripts(&self) -> Vec<PathBuf> {
        transcripts_in(&self.dir)
    }

    pub(crate) fn hook_payloads(&self) -> Option<PathBuf> {
        let p = self.dir.join("hook-payloads.jsonl");
        p.is_file().then_some(p)
    }

    /// Every committed byte of this capture, for the rules that read content
    /// rather than metadata.
    pub(crate) fn wire_files(&self) -> Vec<PathBuf> {
        let mut out = self.transcripts();
        out.extend(self.hook_payloads());
        out
    }
}

/// LOUD, not swallowing: an unreadable capture dir used to make `wire_files()`
/// return empty, so `a_recorded_capture_that_was_edited_says_so` saw no
/// sentinels and passed vacuously for that capture.
fn push_transcripts(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in read_dir_or_panic(dir) {
        let p = e.path();
        if p.is_dir() {
            push_transcripts(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("jsonl")
            && p.file_name().and_then(|s| s.to_str()) != Some("hook-payloads.jsonl")
        {
            out.push(p);
        }
    }
}

/// The non-hook `.jsonl` under a dir, recursively. THE collector — `conformance.rs`
/// used to keep a byte-identical copy with a different error policy.
pub(crate) fn transcripts_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    push_transcripts(dir, &mut out);
    out.sort();
    out
}

pub(crate) fn transcript_in(dir: &Path) -> PathBuf {
    let mut t = transcripts_in(dir);
    assert_eq!(t.len(), 1, "{} must ship one transcript", dir.display());
    t.remove(0)
}

/// A fixture's JSONL payload as its non-empty lines — this binary's one line
/// reader. One definition so a semantics change (a BOM skip, say) cannot reach
/// one source's tests and miss its siblings (#936's diverging-copies class).
pub(crate) fn fixture_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn sources_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sources")
}

/// The conformance subtree, whose scenarios the decode harness iterates.
pub(crate) fn fixtures_root() -> PathBuf {
    sources_root().join("fixtures")
}

pub(crate) fn read_dir_or_panic(dir: &Path) -> Vec<std::fs::DirEntry> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|e| e.unwrap_or_else(|e| panic!("read_dir entry under {}: {e}", dir.display())))
        .collect()
}

pub(crate) fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = read_dir_or_panic(dir)
        .into_iter()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

/// THE walk. Every capture the layout names, each REQUIRED to declare itself.
/// Sorted, so failures name captures in a stable order.
pub(crate) fn every_capture() -> Vec<Capture> {
    let mut out = Vec::new();
    for pixtuoid_core::harness::Capture { dir, source } in
        pixtuoid_core::harness::captures(&sources_root())
    {
        let prov = dir.join("provenance.json");
        assert!(
            prov.is_file(),
            "{}: a capture must declare its origin — add provenance.json here, or \
             one per sub-tree. Nothing else in the bytes separates a capture from a \
             composition, which is the whole reason provenance exists.",
            dir.display()
        );
        let body = std::fs::read_to_string(&prov)
            .unwrap_or_else(|e| panic!("read {}: {e}", prov.display()));
        let provenance: serde_json::Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("{}: not valid JSON: {e}", prov.display()));
        out.push(Capture {
            dir,
            source,
            provenance_path: prov,
            provenance,
        });
    }
    out
}

/// The completeness pin that makes the single walk load-bearing: a capture the
/// walk cannot see is one every rule silently skips, which is the failure the
/// walk exists to make impossible.
#[test]
fn the_walk_sees_every_provenance_on_disk() {
    fn count(dir: &Path, n: &mut usize) {
        for e in read_dir_or_panic(dir) {
            let p = e.path();
            if p.is_dir() {
                count(&p, n);
            } else if p.file_name().and_then(|s| s.to_str()) == Some("provenance.json") {
                *n += 1;
            }
        }
    }
    let mut on_disk = 0;
    count(&sources_root(), &mut on_disk);
    // Derived DIFFERENTLY on purpose: `every_capture()` from the LAYOUT, the
    // counter above from the FILES — one predicate counted twice is a tautology.
    let walked = every_capture().len();
    assert_eq!(
        walked, on_disk,
        "the layout names {walked} captures but {on_disk} provenance.json files exist — \
         a file outside every layout shape is one no rule applies to"
    );
    // Provenance counting cannot see an orphan that declares NOTHING, so account
    // for the WIRE files too: each must belong to a capture the walk returned.
    let mut wire_on_disk: BTreeSet<PathBuf> = BTreeSet::new();
    fn wire(dir: &Path, out: &mut BTreeSet<PathBuf>) {
        for e in read_dir_or_panic(dir) {
            let p = e.path();
            if p.is_dir() {
                wire(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                out.insert(p);
            }
        }
    }
    wire(&sources_root(), &mut wire_on_disk);
    let claimed: BTreeSet<PathBuf> = every_capture()
        .iter()
        .flat_map(|c| {
            let mut fs = transcripts_in(&c.dir);
            fs.extend(c.hook_payloads());
            fs
        })
        .collect();
    let orphans: Vec<String> = wire_on_disk
        .difference(&claimed)
        .map(|p| {
            p.strip_prefix(sources_root())
                .unwrap_or(p)
                .display()
                .to_string()
        })
        .collect();
    assert!(
        orphans.is_empty(),
        "wire bytes no capture claims — a `.jsonl` dropped straight into \
         `fixtures/<source>/`, or a whole unregistered `fixtures/<name>/` dir, \
         declares no provenance and so nothing decodes or scans it:\n  {}",
        orphans.join("\n  ")
    );
    // The PREMISE, not a hand-picked count floor: three sibling floors for this
    // one idea were each a fixture cleanup away from firing and blaming the walk.
    let covered: BTreeSet<String> = every_capture().into_iter().map(|c| c.source).collect();
    let bare: Vec<&str> = registry::registered_source_names()
        .filter(|n| !covered.contains(*n))
        .collect();
    assert!(
        bare.is_empty(),
        "registered sources with no capture at all: {bare:?} — every rule below is \
         silent for them, which is what makes the per-source statement, not a count \
         of captures, the thing that keeps those rules non-vacuous"
    );
}

/// Every capture's layout resolves to a REGISTERED source. A dir name that
/// matches no source is a typo that would otherwise route the capture's rules
/// to a descriptor lookup that silently returns `None`.
#[test]
fn every_captures_layout_names_a_registered_source() {
    for c in every_capture() {
        assert!(
            registry::descriptor_for(&c.source).is_some(),
            "{}: layout names source {:?}, which is not registered",
            c.dir.display(),
            c.source
        );
    }
}

// ── The rules: each an assertion over `every_capture()`, never a subset. ─────

/// `unknown` scenarios that stay unknown, with the reason. It only SHRINKS.
const UNVERIFIED_PROVENANCE: &[&str] = &[
    // Not re-recordable: its cwd is a real Windows path with a space and parens,
    // which is what makes it the Windows arm of the cwd-extractor test.
    "fixtures/copilot/tool-run",
];

/// Sources whose `unknown` scenarios now sit BESIDE a recorded one.
const UNKNOWN_BUT_BACKED_BY_A_CAPTURE: &[&str] = &["copilot"];

/// Hook-only sources with no recorded scenario. The list only SHRINKS — a hook
/// event is transient, so for these the fixture is the only wire evidence there
/// will ever be, and a new hook-only CLI must not join it by default.
const NO_WIRE_EVIDENCE_YET: &[&str] = &[];

/// `provenance.schema.json` — the ONE statement of what each origin requires,
/// read by this gate and the README table.
struct ProvenanceSchema(serde_json::Value);

impl ProvenanceSchema {
    fn load() -> Self {
        let p = fixtures_root().join("provenance.schema.json");
        let body =
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
        Self(serde_json::from_str(&body).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
    }

    fn origins(&self) -> &serde_json::Map<String, serde_json::Value> {
        self.0["origins"].as_object().expect("origins is an object")
    }

    fn required(&self, origin: &str) -> Option<Vec<String>> {
        Some(
            self.origins().get(origin)?["required"]
                .as_array()
                .expect("required is an array")
                .iter()
                .map(|v| {
                    v.as_str()
                        .expect("a required member is a string")
                        .to_string()
                })
                .collect(),
        )
    }
}

/// Every scenario declares where its bytes came from, because nothing IN them
/// separates a capture from a composition — a redacted cwd and an invented one
/// look alike. A composed fixture pins its author's belief and the decoder then
/// agrees with it: kimi's shipped four confident per-call ids for a field kimi
/// never sends.
#[test]
fn every_capture_declares_a_valid_origin_with_its_required_fields() {
    let schema = ProvenanceSchema::load();
    for c in every_capture() {
        let required = schema.required(c.origin()).unwrap_or_else(|| {
            panic!(
                "{}: origin {:?} is not recorded | composed | unknown",
                c.provenance_path.display(),
                c.origin()
            )
        });
        for key in &required {
            assert!(
                c.field(key).is_some_and(|s| !s.trim().is_empty()),
                "{}: origin {:?} requires a non-empty {key:?}",
                c.provenance_path.display(),
                c.origin()
            );
        }
    }
}

/// `cli` was the one required field nothing could falsify, so a provenance
/// naming a DIFFERENT CLI than the tree it sits in passed every gate. The
/// registry's probe argv[0] is the name the user types (`agy`, `cursor-agent`),
/// which is what a capture command starts with.
#[test]
fn a_recorded_captures_cli_is_its_trees_binary() {
    for c in every_capture().iter().filter(|c| c.is_recorded()) {
        let Some(probe) = registry::descriptor_for(&c.source).and_then(|d| d.version_probe) else {
            continue;
        };
        let Some(cli) = c.field("cli") else { continue };
        assert_eq!(
            cli,
            probe[0],
            "{}: `cli` is {cli:?} but this tree is {}, whose binary is {:?}",
            c.provenance_path.display(),
            c.source,
            probe[0]
        );
    }
}

/// Every capture exempt from the strip, by its layout path. Pinned as an exact
/// set so an exemption is a reviewed diff here, never a quiet key in a record:
/// an exempt capture is the one path inventory data the gitleaks backstop
/// cannot see has back into the tree.
const EXEMPT_FROM_STRIP: &[&str] = &["cursor/fixtures"];

/// Keys whose VALUE is an operator's account rather than the wire's content. Not
/// a guess at what a vendor might name one — the strip already removes every field
/// no decoder reads, so this is a tripwire on the names that have actually reached
/// a fixture, asserting the invariant the strip provides.
const IDENTITY_KEYS: &[&str] = &[
    "account_id",
    "accountId",
    "api_key",
    "email",
    "token",
    "user_email",
    "userEmail",
    "user_id",
];

/// Collect every `IDENTITY_KEYS` entry holding something, at any depth.
fn filled_identity_keys(v: &serde_json::Value, out: &mut BTreeSet<String>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, child) in m {
                let empty = matches!(child, serde_json::Value::Null)
                    || *child == serde_json::json!("")
                    || *child == serde_json::json!(0)
                    || *child == serde_json::json!(false)
                    || *child == serde_json::json!([])
                    || *child == serde_json::json!({});
                if !empty && IDENTITY_KEYS.contains(&k.as_str()) {
                    out.insert(k.clone());
                }
                filled_identity_keys(child, out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|c| filled_identity_keys(c, out)),
        _ => {}
    }
}

/// Outside the pinned exemptions, no identity key holds a value — the invariant
/// the strip provides, asserted on the bytes rather than trusted. It fires on the
/// three ways one could come back: an exempt capture nobody reviewed, a hand edit,
/// and a decoder that starts reading such a field so the strip keeps it.
#[test]
fn no_identity_key_holds_a_value_outside_a_pinned_exemption() {
    let mut probe = BTreeSet::new();
    filled_identity_keys(
        &serde_json::json!({"a": {"user_id": "u_1"}, "b": [{"token": ""}]}),
        &mut probe,
    );
    assert_eq!(
        probe.iter().map(String::as_str).collect::<Vec<_>>(),
        ["user_id"],
        "the walker must see a filled key at depth and ignore a blanked one"
    );

    for c in every_capture() {
        let label = c
            .dir
            .strip_prefix(sources_root())
            .expect("under root")
            .to_string_lossy()
            .replace('\\', "/");
        if EXEMPT_FROM_STRIP.contains(&label.as_str()) {
            continue;
        }
        for wire in c.wire_files() {
            let body = std::fs::read_to_string(&wire).expect("read");
            let mut found = BTreeSet::new();
            for (i, line) in body
                .lines()
                .enumerate()
                .filter(|(_, l)| !l.trim().is_empty())
            {
                // A line that will not parse is the input `strip_unread` declines a
                // whole file for, so skipping it here would leave those bytes both
                // unstripped and unchecked. Loud instead: fix the line, or declare
                // the capture exempt and say why.
                let v: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
                    panic!(
                        "{}:{}: not JSON ({e}), so the strip skipped this whole file",
                        wire.display(),
                        i + 1
                    )
                });
                filled_identity_keys(&v, &mut found);
            }
            assert!(
                found.is_empty(),
                "{}: {found:?} hold values. The strip blanks what no decoder reads, so \
                 either a decoder now reads one — re-record and redact by hand — or these \
                 bytes never went through it",
                wire.display()
            );
        }
    }
}

/// The strip is disclosed by `deidentified`; a note that still calls the bytes
/// verbatim contradicts it, and an exemption without its reason is a silent hole.
#[test]
fn a_stripped_record_does_not_call_its_bytes_verbatim_and_an_exemption_says_why() {
    let mut exempt = BTreeSet::new();
    for c in every_capture() {
        let prov = c.provenance_path;
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&prov).expect("read")).expect("json");
        let blanked = v["deidentified"]["blanked"].as_u64().unwrap_or(0);
        let note = v["note"].as_str().unwrap_or("");
        assert!(
            blanked == 0 || !note.to_ascii_lowercase().contains("verbatim"),
            "{}: {blanked} subtrees were blanked, and the note still says verbatim: {note:?}",
            prov.display()
        );
        if v["deidentified"]["method"] == "none" {
            assert!(
                v["deidentified"]["why"]
                    .as_str()
                    .is_some_and(|w| !w.trim().is_empty()),
                "{}: exempt from the strip without saying why",
                prov.display()
            );
            exempt.insert(
                c.dir
                    .strip_prefix(sources_root())
                    .expect("under root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    let pinned: BTreeSet<String> = EXEMPT_FROM_STRIP.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        exempt, pinned,
        "the exempt set changed — review it here, not only in a record"
    );
}

/// A `recorded` fixture whose bytes were EDITED must say so: nothing in them
/// separates a capture from a composition — the whole reason provenance exists —
/// so a redaction sentinel with a silent `note` is the one state the mechanism
/// cannot tolerate. Sentinels, not a `/Users/dev` grep: the sweep that keyed on
/// that alone missed kimi's, whose redaction is a captured `ls -la`'s owner column.
/// HAND edits only: the recorder's own strip plants no sentinel and discloses
/// itself through `deidentified` in the provenance, not through `note`.
#[test]
fn a_recorded_capture_that_was_edited_says_so() {
    // Placeholders READ from the scanner's own allowlist, never copied: a hand-copy
    // knew only `/Users/dev`, so a `/home/dev` redaction was edited-but-silent here.
    let allow = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.gitleaks-identity.toml"),
    )
    .expect(".gitleaks-identity.toml");
    // Both the path rule and the dash-encoded rule carry the placeholder
    // alternation; they must AGREE, or whichever this positional read lands on
    // silently narrows the sentinel set.
    let mut alternations: Vec<&str> = allow
        .split("(?:dev|")
        .skip(1)
        .filter_map(|s| s.split(')').next())
        .collect();
    alternations.dedup();
    assert_eq!(
        alternations.len(),
        1,
        "the placeholder alternations diverged across rules — re-align them: {alternations:?}",
    );
    let names = *alternations.first().expect(
        "the placeholder alternation moved — re-derive this from the rule. Only \
         the PLACEHOLDER lines: the sibling lines name infrastructure accounts a \
         real UNEDITED capture can carry, and reading those made an honest note \
         look like a silent one",
    );
    let mut sentinels: Vec<String> = Vec::new();
    for root in ["/Users/", "/home/"] {
        for who in std::iter::once("dev").chain(names.split('|')) {
            sentinels.push(format!("{root}{who}"));
        }
    }
    sentinels.extend([" dev  wheel", " dev  staff", "[redacted", "dev@"].map(String::from));
    // A hand-placed placeholder is a redaction's own signature, and the path and
    // address shapes above cannot see one: nine fixtures had their plugin roster
    // replaced this way while this test stayed green. The recorder's strip removes
    // a roster now, so these are dormant rather than dead — a hand edit can still
    // write them, and that edit is what must disclose itself.
    sentinels.extend(
        [
            "example-skill",
            "example-agent",
            "example-plugin",
            "mcp__example",
        ]
        .map(String::from),
    );
    // A path sentinel must end at a real boundary: bare `contains` let a longer
    // account name that merely STARTS with a placeholder excuse itself.
    let hit = |body: &str, s: &str| {
        body.match_indices(s).any(|(at, _)| {
            !s.starts_with('/')
                || body[at + s.len()..].chars().next().is_none_or(
                    |c| !matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-'),
                )
        })
    };
    // A word ending the clause right before "redact" that inverts it.
    const NEGATIONS: &[&str] = &["no", "not", "nothing", "never", "without"];
    let mut silent = Vec::new();
    for c in every_capture().iter().filter(|c| c.is_recorded()) {
        let note = c.field("note").unwrap_or_default().to_ascii_lowercase();
        // "verbatim" USED to satisfy this, and a bare `contains` re-opens that hole
        // one word over ("unredacted"). "sanitiz" too — the oldest captures use it.
        let declares = ["redact", "sanitiz"].iter().any(|stem| {
            note.match_indices(stem).any(|(at, _)| {
                let before = note[..at].trim_end();
                !before.ends_with("un") && !NEGATIONS.iter().any(|n| before.ends_with(n))
            })
        });
        let edited = c.wire_files().iter().any(|p| {
            std::fs::read_to_string(p)
                .map(|b| sentinels.iter().any(|s| hit(&b, s)))
                .unwrap_or(false)
        });
        if edited && !declares {
            silent.push(c.dir.strip_prefix(sources_root()).unwrap().to_path_buf());
        }
    }
    assert!(
        silent.is_empty(),
        "these `recorded` fixtures carry a redaction sentinel and no `note` claiming a \
         redaction — a reader cannot tell an edited capture from an untouched one, and a \
         note calling these bytes verbatim is worse than a silent one: {silent:?}"
    );
}

/// `verified_version` means "the version whose wire we have SEEN". A recorded
/// scenario IS that sighting, so the two must not disagree — `doctor`'s "newer
/// than verified" warning is structurally silent at `unknown` and would
/// otherwise warn a user running the exact version we hold a capture from.
#[test]
fn a_recorded_capture_anchors_its_sources_verified_version() {
    let mut by_source: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    let mut recorded_at_all: BTreeSet<String> = BTreeSet::new();
    for c in every_capture().iter().filter(|c| c.is_recorded()) {
        recorded_at_all.insert(c.source.clone());
        let version = c.field("version").unwrap_or_default();
        if version.chars().any(|ch| ch.is_ascii_digit()) {
            by_source.entry(c.source.clone()).or_default().push((
                c.field("captured").unwrap_or_default().to_string(),
                version.to_string(),
            ));
        }
    }
    for source in recorded_at_all {
        let Some(d) = registry::descriptor_for(&source) else {
            continue;
        };
        let Some(versions) = by_source.get(&source) else {
            // Skipping a version-less source is how `verified_version:
            // "0.0.0-A-LIE"` passed for copilot: three scenarios, all `unknown`.
            assert_eq!(
                d.verified_version, "unknown",
                "{source}: every recorded capture here says `version: unknown`, so nothing \
                 in this tree anchors {:?}. Re-record with a version, or set the field to \
                 \"unknown\" — a number no capture backs is the state the field was defined \
                 to avoid.",
                d.verified_version
            );
            continue;
        };
        // The HIGHEST version: `any` let hermes' 0.20.0 anchor beside its 0.20.1,
        // and the newest DATE would move the pin DOWN on a stale machine.
        let dated: Vec<&(String, String)> = versions
            .iter()
            .filter(|(when, _)| is_iso_date(when))
            .collect();
        assert!(
            !dated.is_empty(),
            "{source}: has recorded versions but none with an ISO `captured`, so nothing \
             dates the sightings — every version-bearing capture carries one, and a \
             source that loses it must fail loudly rather than weaken the rule"
        );
        // Version first, `captured` only as TIE-BREAK: without it two captures of one
        // version resolve by dir iteration order.
        let version = |s: &str| registry::parse_version(s, d.version_hyphen);
        let highest = &dated
            .iter()
            .max_by(|a, b| {
                let precedence = match (version(&a.1), version(&b.1)) {
                    (Some(a), Some(b)) => a.cmp_precedence(&b),
                    (a, b) => a.is_some().cmp(&b.is_some()),
                };
                precedence.then_with(|| a.0.cmp(&b.0))
            })
            .expect("non-empty")
            .1;
        let pinned = version(highest);
        assert!(
            pinned
                .as_ref()
                .zip(version(d.verified_version))
                .is_some_and(|(p, v)| p.cmp_precedence(&v).is_eq()),
            "{source}: `verified_version` is {:?}, but the highest recorded capture \
             ({highest:?}) pins {:?} — the newest wire we hold is the one that counts",
            d.verified_version,
            pinned.map_or_else(|| "nothing".to_owned(), |v| v.to_string())
        );
    }
}

/// The three pinned rosters, over the SAME walk the rules use — they used to be
/// computed inside one rule's own narrower walk.
#[test]
fn the_pinned_provenance_rosters_hold_both_ways() {
    let captures = every_capture();
    let recorded: BTreeSet<&str> = captures
        .iter()
        .filter(|c| c.is_recorded())
        .map(|c| c.source.as_str())
        .collect();
    // Keyed off `sources_root()`, not `fixtures_root()`: the `?` on the narrower
    // prefix silently dropped every single-owner capture from this rule.
    let unknown: BTreeSet<String> = captures
        .iter()
        .filter(|c| c.origin() == "unknown")
        .map(|c| {
            c.dir
                .strip_prefix(sources_root())
                .expect("every capture is under tests/sources")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(
        unknown,
        UNVERIFIED_PROVENANCE
            .iter()
            .map(|s| (*s).to_string())
            .collect::<BTreeSet<_>>(),
        "the `unknown` set is pinned — record one and drop its entry, or explain a new \
         fixture as recorded|composed"
    );
    // BOTH directions: the one-way check let two members survive after their
    // `unknown` scenarios were recorded away.
    let unknown_sources: BTreeSet<&str> = UNVERIFIED_PROVENANCE
        .iter()
        .filter_map(|s| {
            s.strip_prefix("fixtures/")
                .and_then(|r| r.split('/').next())
        })
        .collect();
    for src in UNKNOWN_BUT_BACKED_BY_A_CAPTURE {
        assert!(
            recorded.contains(src),
            "{src} is listed as backed by a capture but has no recorded scenario"
        );
        assert!(
            unknown_sources.contains(src),
            "{src} has no `unknown` scenario left — drop its \
             UNKNOWN_BUT_BACKED_BY_A_CAPTURE entry"
        );
    }
    for src in registry::registered_source_names()
        .filter(|s| registry::descriptor_for(s).is_some_and(|d| d.line_decoder().is_none()))
    {
        assert_eq!(
            NO_WIRE_EVIDENCE_YET.contains(&src),
            !recorded.contains(src),
            "{src} is hook-only with recorded={}; either record a scenario and drop its \
             NO_WIRE_EVIDENCE_YET entry, or add one",
            recorded.contains(src)
        );
    }
}

/// Moving the required list into a DATA file made every provenance gate editable
/// as prose: emptying `required` and updating the README to match disarms both
/// gates and reads as documentation maintenance. Widening is free; narrowing has
/// to come through here.
#[test]
fn the_schema_cannot_be_narrowed_by_a_data_edit() {
    let schema = ProvenanceSchema::load();
    for (origin, must) in [
        ("recorded", &["cli", "version", "captured", "command"][..]),
        ("composed", &["note"][..]),
        ("unknown", &["note"][..]),
    ] {
        let got: BTreeSet<String> = schema
            .required(origin)
            .unwrap_or_else(|| panic!("{origin} missing from the schema"))
            .into_iter()
            .collect();
        let want: BTreeSet<String> = must.iter().map(|s| (*s).to_string()).collect();
        assert!(
            got.is_superset(&want),
            "provenance.schema.json narrowed {origin} to {got:?} — it must still require \
             at least {want:?}"
        );
    }
}

/// The README's table is prose a human reads instead of the JSON, so it is the
/// copy most likely to rot.
#[test]
fn the_readme_states_the_schema_the_gates_enforce() {
    let schema = ProvenanceSchema::load();
    let readme = std::fs::read_to_string(fixtures_root().join("README.md")).expect("README");
    for (origin, spec) in schema.origins() {
        let fields = schema.required(origin).expect("origin in its own table");
        let row = format!(
            "| `{origin}` | {} | {} |",
            fields
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", "),
            spec["means"].as_str().expect("means is a string")
        );
        assert!(
            readme.contains(&row),
            "fixtures/README.md must carry this row verbatim:\n  {row}"
        );
    }
}

/// A `recorded` provenance must be FALSIFIABLE by its own bytes; one that is not
/// is a claim, not a record — a wholly false one (`cli: codex`, `version:
/// 0.0.0-A-LIE`) passed every gate in this tree until an equivalent of this ran.
/// Three axes the committed bytes can answer, plus the field shapes: `cli` is
/// `a_recorded_captures_cli_is_its_trees_binary`, this is everything else.
#[test]
fn a_recorded_captures_claims_are_falsified_by_its_own_bytes() {
    let mut bad: Vec<String> = Vec::new();
    for c in every_capture().iter().filter(|c| c.is_recorded()) {
        let at = c.provenance_path.display();

        // A `version` holding the INVOCATION reports a drift of nothing for as long
        // as it sits there — the live instance was `"grok --permission-mode default"`.
        let version = c.field("version").unwrap_or_default();
        if !version.is_empty()
            && version != "unknown"
            && !version.chars().any(|ch| ch.is_ascii_digit())
        {
            bad.push(format!(
                "{at}: `version` carries no version number: {version:?}"
            ));
        }

        let captured = c.field("captured").unwrap_or_default();
        if !captured.is_empty() && captured != "unknown" && !is_iso_date(captured) {
            bad.push(format!("{at}: `captured` is not an ISO date: {captured:?}"));
        }

        let mut stamps: BTreeSet<String> = BTreeSet::new();
        let mut dates: BTreeSet<String> = BTreeSet::new();
        if let Some(hooks) = c.hook_payloads() {
            for line in fixture_lines(&hooks) {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if let Some(src) = v.get("_pixtuoid_source").and_then(|s| s.as_str()) {
                    stamps.insert(src.to_string());
                }
                if let Some(ms) = v.get("_shim_ts_ms").and_then(serde_json::Value::as_i64) {
                    dates.insert(utc_date(ms));
                }
            }
        }
        // codex and omp name transcripts by capture instant — the only date evidence
        // a transcript-only fixture has, and most captures ship no hook payloads.
        for f in c.wire_files() {
            if let Some(d) = date_prefix(&f.file_name().unwrap_or_default().to_string_lossy()) {
                dates.insert(d);
            }
        }

        if stamps.len() > 1 {
            bad.push(format!(
                "{at}: payloads carry TWO sources ({stamps:?}) — a capture scooped up another CLI"
            ));
        } else if let Some(only) = stamps.iter().next() {
            // A single WRONG stamp is answerable wherever the layout names the
            // source, and BOTH layouts do.
            if *only != c.source {
                bad.push(format!(
                    "{at}: payloads are stamped `{only}` but this tree is `{}`",
                    c.source
                ));
            }
        }
        if !dates.is_empty()
            && !captured.is_empty()
            && captured != "unknown"
            && !dates.contains(captured)
        {
            bad.push(format!(
                "{at}: `captured` {captured} contradicts the stamps in the bytes \
                 ({dates:?}) — the recorder dates in UTC"
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// `YYYY-MM-DD`, validated — not a 10-char string that starts "20".
fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    // `u32::from_str` accepts a leading sign, so "+026-08-15" parsed as year 26.
    if b.iter()
        .enumerate()
        .any(|(i, c)| i != 4 && i != 7 && !c.is_ascii_digit())
    {
        return false;
    }
    let num = |r: std::ops::Range<usize>| s[r].parse::<u32>().ok();
    let (Some(y), Some(m), Some(d)) = (num(0..4), num(5..7), num(8..10)) else {
        return false;
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let last = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=last).contains(&d)
}

/// The UTC calendar date of a millisecond epoch stamp. Days-from-epoch, because
/// the shim stamps UTC and `captured` is UTC.
fn utc_date(ms: i64) -> String {
    let mut days = ms.div_euclid(86_400_000);
    let (mut y, mut m) = (1970i64, 1i64);
    // A pre-epoch stamp is not a real `_shim_ts_ms`, but a corrupt one must still
    // render a well-formed date — the forward walk alone emitted a malformed one.
    while days < 0 {
        y -= 1;
        days += if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
            366
        } else {
            365
        };
    }
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let len = if leap { 366 } else { 365 };
        if days < len {
            break;
        }
        days -= len;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    loop {
        let len = match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            _ => 28,
        };
        if days < len {
            break;
        }
        days -= len;
        m += 1;
    }
    format!("{y:04}-{m:02}-{:02}", days + 1)
}

/// The `YYYY-MM-DD` a CLI put at the head of a transcript filename, with codex's
/// `rollout-` prefix: `rollout-2026-08-14T22-19-03-<uuid>.jsonl`.
fn date_prefix(name: &str) -> Option<String> {
    let rest = name.strip_prefix("rollout-").unwrap_or(name);
    let (date, tail) = rest.split_at_checked(10)?;
    if !tail.starts_with('T') || !is_iso_date(date) {
        return None;
    }
    Some(date.to_string())
}

/// `utc_date` and `is_iso_date` are hand-rolled (this crate has no chrono), so
/// they get the cases that break naive versions: a leap day, the day after, a
/// non-leap Feb 29, and the epoch itself.
#[test]
fn the_date_helpers_handle_the_cases_that_break_naive_ones() {
    assert_eq!(utc_date(0), "1970-01-01");
    // Pre-epoch: impossible for a real stamp, but a corrupt one must not put a
    // malformed string into the diagnostic that reports it.
    assert_eq!(utc_date(-1), "1969-12-31");
    assert_eq!(utc_date(-86_400_001), "1969-12-30");
    assert!(!is_iso_date("+026-08-15"), "a signed year is not ISO");
    assert_eq!(utc_date(1_786_854_010_213), "2026-08-16");
    // 2024-02-29T00:00:00Z — a real leap day, and the day after it.
    assert_eq!(utc_date(1_709_164_800_000), "2024-02-29");
    assert_eq!(utc_date(1_709_251_200_000), "2024-03-01");
    assert!(is_iso_date("2024-02-29"), "a real leap day");
    assert!(!is_iso_date("2026-02-29"), "2026 is not a leap year");
    assert!(!is_iso_date("2026-13-01") && !is_iso_date("2026-00-10"));
    assert!(!is_iso_date("2026-08-32") && !is_iso_date("2026-08-00"));
    assert!(!is_iso_date("2026/08/15") && !is_iso_date("20260815") && !is_iso_date("unknown"));
    assert_eq!(
        date_prefix("rollout-2026-08-14T22-19-03-01a0.jsonl").as_deref(),
        Some("2026-08-14")
    );
    assert_eq!(
        date_prefix("2026-08-15T04-14-55-304Z_01a0.jsonl").as_deref(),
        Some("2026-08-15")
    );
    assert_eq!(date_prefix("events.jsonl"), None);
    assert_eq!(date_prefix("2026-13-45T00-00-00-x.jsonl"), None);
}

/// Reading the wrong `tool_id_key` is SILENT — every kimi tool call decoded to
/// `None` for the source's whole life, because a NEW source's conformance
/// snapshot is generated from whatever key its author copied. Whatever key a real
/// payload carries its tool id under IS that source's key; exemption is by NAME
/// only, never by an inferred property (`custom` claiming all is intent, not state).
const TOOL_ID_KEY_UNPROVEN: &[&str] = &[
    // `inert: custom claims all` in their registry row — the shared arms never run.
    "codewhale",
    "dsh",
    "hermes",
    "omp",
    "opencode",
    "reasonix",
    // LIVE (`custom: None`), just unexercised: a hook spec with no install target,
    // so nothing can send us one — the only entry here a capture would remove.
    "antigravity",
];

#[test]
fn each_sources_tool_id_key_is_the_one_its_captures_carry() {
    const KEYS: &[&str] = &["tool_use_id", "tool_call_id"];
    let mut proven: BTreeSet<String> = BTreeSet::new();
    for c in every_capture() {
        let Some(hooks) = c.hook_payloads() else {
            continue;
        };
        let Some(d) = registry::descriptor_for(&c.source) else {
            continue;
        };
        let Some(want) = d.hook().map(|h| h.tool_id_key.wire_name()) else {
            continue;
        };
        for line in fixture_lines(&hooks) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            for wrong in KEYS.iter().filter(|k| **k != want) {
                assert!(
                    v.get(wrong).is_none(),
                    "{}: a payload carries `{wrong}` but {} is registered as \
                     `{want}` — the per-call id would decode to None for every \
                     tool call this source ever makes",
                    hooks.display(),
                    c.source
                );
            }
            if v.get(want).is_some() {
                proven.insert(c.source.clone());
            }
        }
    }
    let mut wrong: Vec<String> = Vec::new();
    for name in registry::registered_source_names() {
        let Some(d) = registry::descriptor_for(name) else {
            continue;
        };
        if d.hook().is_none() {
            continue;
        }
        match (proven.contains(name), TOOL_ID_KEY_UNPROVEN.contains(&name)) {
            (false, false) => wrong.push(format!("{name}: add a capture or roster it")),
            (true, true) => wrong.push(format!("{name}: proven — drop it from the roster")),
            _ => {}
        }
    }
    assert!(
        wrong.is_empty(),
        "a hook row's tool_id_key is proven by real bytes or it names why it cannot \
         be, never both and never neither:\n  {}",
        wrong.join("\n  ")
    );
}
